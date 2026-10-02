//! The overlay edge: how a hidden-service connector is reached, and how it reaches out
//! (ADR 0003).
//!
//! An edge issues each connector an onion endpoint that is the same every time, publishes
//! the local ports behind it, and provides the SOCKS proxy that all of the connector's
//! outbound traffic goes through, settlement RPC included. Two implementations are held to
//! one contract suite, in the tests below. The real one runs the `anon` daemon (`anon.rs`);
//! when it cannot bootstrap, every command that needs an overlay fails, as it must. The
//! other is the loopback stand-in the tests use: `TOON_OVERLAY=loopback` selects it, and
//! its proxy passes through to loopback addresses and to the ports published behind an
//! onion endpoint, and refuses everything else.
//!
//! Both are found by the next process. The daemon leaves its port in `overlay/daemon`; the
//! stand-in leaves where its proxy listens in `overlay/loopback`, and a later `bootstrap`
//! uses that proxy for as long as it answers, so a command reaches what the supervisor
//! published. A stand-in found that way publishes nothing of its own: what is published is
//! held by the process that started it.
//!
//! One test seam is beside `TOON_OVERLAY=loopback`, and has no effect on the real overlay:
//! `TOON_OVERLAY_NAMES` is a comma-separated list of `<name>.anyone:<port>=<ip>:<port>`,
//! each telling the stand-in that a `.anyone` name and port stand for a loopback address,
//! so that a fake relay or connector a test started is reachable at a `.anyone` address.
//! The proxy that honours it is the one the list was in the environment of when it started.
//! An address that is not loopback is ignored.
//!
//! A connector's onion endpoint is made from the key the wallet derives for it
//! (`derive::onion_secret`), which is part of the wallet's backup.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sha3::{Digest, Sha3_256};

use crate::outcome::{Error, ErrorCode};

/// The environment variable that swaps the real overlay for the loopback stand-in.
pub const OVERLAY_VARIABLE: &str = "TOON_OVERLAY";

/// The environment variable that tells the loopback stand-in which `.anyone` names stand
/// for which loopback addresses (see the module's documentation).
pub const NAMES_VARIABLE: &str = "TOON_OVERLAY_NAMES";

/// The top-level domain Anyone's daemon publishes.
pub const TLD: &str = "anyone";

/// The port of an onion endpoint that the connector is served on.
pub const CONNECTOR_PORT: u16 = 80;

/// The port of an onion endpoint that the relay's read port is served on: the port the
/// relay serves on inside its container, so the endpoint answers on the same number a
/// relay does anywhere else.
pub const RELAY_READ_PORT: u16 = 7100;

pub trait Edge: Send + Sync {
    /// Where outbound traffic is proxied: a SOCKS5 proxy on this machine.
    fn proxy(&self) -> SocketAddr;

    /// The onion endpoint of the connector numbered `connector`, whose key is the file
    /// `key`. The same every time it is asked, and after a restart.
    fn issue(&self, connector: u32, key: &Path) -> Result<String, Error>;

    /// Serve `ports` at the onion endpoint `address`, each at its own port, from the
    /// local address behind it. Replaces what was published for that endpoint before.
    fn publish(&self, address: &str, ports: &[(u16, SocketAddr)]) -> Result<(), Error>;

    /// The overlay is no longer needed on this machine: a daemon that nothing else uses
    /// stops. The stand-in has nothing to stop.
    fn release(&self) {}
}

/// Whether `url` is a plain `http://` or `ws://` endpoint on this machine (`localhost`,
/// `127.0.0.1`, `[::1]`). On a hidden agent node it is the one kind of request that is not
/// sent through the overlay's proxy: it has nothing to hide, and the real daemon refuses
/// private addresses. It is the rule for the connector's rendered config and for every
/// request a command makes.
pub fn is_local_plain(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("ws://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.find(']') {
        Some(end) => &authority[..=end],
        None => authority.split(':').next().unwrap_or(authority),
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn unavailable(why: &str) -> Error {
    Error {
        code: ErrorCode::OverlayUnavailable,
        message: format!(
            "The Anyone overlay did not bootstrap: {why}. A hidden service is never created \
             without it, and nothing falls back to clearnet; `--clearnet <hostname>` asks for \
             clearnet explicitly."
        ),
    }
}

/// Bootstrap the overlay of the agent node at `home`, or say why it did not. `agreed` is
/// whether the operator has just agreed to Anyone's terms; the daemon does not start
/// without that, now or on record.
pub fn bootstrap(home: &Path, agreed: bool) -> Result<Box<dyn Edge>, Error> {
    match env::var(OVERLAY_VARIABLE).ok().as_deref() {
        Some("loopback") => Ok(Box::new(Loopback::start(home).map_err(|error| {
            unavailable(&format!("the stand-in proxy could not listen: {error}"))
        })?)),
        Some(other) if !other.is_empty() => Err(unavailable(&format!(
            "{OVERLAY_VARIABLE} names no overlay: {other}"
        ))),
        _ => crate::anon::bootstrap(home, agreed).map(|edge| Box::new(edge) as Box<dyn Edge>),
    }
}

/// The onion endpoint of an Ed25519 key: the key, a checksum and a version, in base32,
/// as Tor's v3 addresses are made and as `anon` writes them. Anyone's checksum is over
/// `.anyone checksum`, not Tor's `.onion checksum`, which the real daemon showed.
pub fn address_of(secret: &[u8; 32]) -> String {
    let public = ed25519_dalek::SigningKey::from_bytes(secret)
        .verifying_key()
        .to_bytes();
    let checksum = Sha3_256::new()
        .chain_update(b".anyone checksum")
        .chain_update(public)
        .chain_update([3u8])
        .finalize();
    let mut bytes = public.to_vec();
    bytes.extend_from_slice(&checksum[..2]);
    bytes.push(3);
    format!("{}.{TLD}", base32(&bytes))
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// What a stand-in's proxy serves from: where it listens and what is published behind it.
type Published = Arc<Mutex<HashMap<(String, u16), SocketAddr>>>;

/// The proxy of a stand-in this process started, which stops when the last of those using
/// it lets go.
struct Listening {
    file: PathBuf,
    proxy: SocketAddr,
    published: Published,
    stopped: Arc<AtomicBool>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Only the file that is still ours: another stand-in may have replaced it.
        if fs::read_to_string(&self.file).is_ok_and(|kept| kept.trim() == self.proxy.to_string()) {
            let _ = fs::remove_file(&self.file);
            // The directory is left only if something else is in it.
            if let Some(dir) = self.file.parent() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

/// The stand-ins this process has started, by agent node, so that a second `bootstrap` in
/// one process shares the first's table of what is published.
static STARTED: Mutex<Vec<(PathBuf, std::sync::Weak<Listening>)>> = Mutex::new(Vec::new());

/// The stand-in: a SOCKS5 proxy on loopback and a table of what is published behind each
/// onion endpoint.
pub struct Loopback {
    dir: PathBuf,
    proxy: SocketAddr,
    /// `None` when the proxy was found, and is another process's.
    own: Option<Arc<Listening>>,
}

/// Whether a SOCKS5 proxy answers at `proxy`.
fn answers(proxy: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&proxy, Duration::from_secs(2)) else {
        return false;
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut answer = [0u8; 2];
    stream.write_all(&[5, 1, 0]).is_ok()
        && stream.read_exact(&mut answer).is_ok()
        && answer == [5, 0]
}

/// The `.anyone` names `list`, the value of `TOON_OVERLAY_NAMES`, says stand for loopback
/// addresses.
fn declared_names(list: &str) -> HashMap<(String, u16), SocketAddr> {
    let mut names = HashMap::new();
    for entry in list.split(',') {
        let parsed = entry.split_once('=').and_then(|(from, to)| {
            let (host, port) = from.trim().rsplit_once(':')?;
            let to: SocketAddr = to.trim().parse().ok()?;
            Some(((host.to_ascii_lowercase(), port.parse().ok()?), to))
        });
        if let Some((from, to)) = parsed.filter(|(_, to)| to.ip().is_loopback()) {
            names.insert(from, to);
        }
    }
    names
}

impl Loopback {
    /// The stand-in of the agent node at `home`: the one this process started, or one that
    /// another process left and that still answers, or a new one.
    pub fn start(home: &Path) -> io::Result<Self> {
        Self::start_naming(home, &env::var(NAMES_VARIABLE).unwrap_or_default())
    }

    /// [`Loopback::start`], with the names a new proxy is told in `names`.
    fn start_naming(home: &Path, names: &str) -> io::Result<Self> {
        let dir = home.join("overlay");
        let file = dir.join("loopback");
        let mut started = STARTED.lock().unwrap_or_else(|e| e.into_inner());
        started.retain(|(_, listening)| listening.strong_count() > 0);
        if let Some(own) = started
            .iter()
            .find(|(kept, _)| *kept == dir)
            .and_then(|(_, listening)| listening.upgrade())
        {
            return Ok(Self {
                dir,
                proxy: own.proxy,
                own: Some(own),
            });
        }
        let found = fs::read_to_string(&file)
            .ok()
            .and_then(|text| text.trim().parse::<SocketAddr>().ok())
            .filter(|proxy| proxy.ip().is_loopback() && answers(*proxy));
        if let Some(proxy) = found {
            return Ok(Self {
                dir,
                proxy,
                own: None,
            });
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let proxy = listener.local_addr()?;
        let published = Arc::new(Mutex::new(declared_names(names)));
        let stopped = Arc::new(AtomicBool::new(false));
        let (table, stop) = (Arc::clone(&published), Arc::clone(&stopped));
        thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let table = Arc::clone(&table);
                        thread::spawn(move || {
                            let _ = socks(stream, &table);
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        let own = Arc::new(Listening {
            file,
            proxy,
            published,
            stopped,
        });
        crate::node::write(&own.file, format!("{proxy}\n").as_bytes(), 0o600)
            .map_err(|error| io::Error::other(error.message))?;
        started.push((dir.clone(), Arc::downgrade(&own)));
        Ok(Self {
            dir,
            proxy,
            own: Some(own),
        })
    }
}

impl Edge for Loopback {
    fn proxy(&self) -> SocketAddr {
        self.proxy
    }

    fn issue(&self, connector: u32, key: &Path) -> Result<String, Error> {
        let secret = zeroize::Zeroizing::new(fs::read(key).map_err(|error| Error {
            code: ErrorCode::Io,
            message: format!("{}: {error}.", key.display()),
        })?);
        let secret: [u8; 32] = secret.as_slice().try_into().map_err(|_| Error {
            code: ErrorCode::Io,
            message: format!("{} is not a 32-byte key.", key.display()),
        })?;
        let address = address_of(&secret);
        // Where `anon` keeps it, so that what the CLI reads is the same file in both.
        crate::node::write(
            &self.dir.join(connector.to_string()).join("hostname"),
            format!("{address}\n").as_bytes(),
            0o600,
        )?;
        Ok(address)
    }

    fn publish(&self, address: &str, ports: &[(u16, SocketAddr)]) -> Result<(), Error> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        let mut table = own.published.lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|(host, _), _| host != address);
        for (port, target) in ports {
            table.insert((address.to_owned(), *port), *target);
        }
        Ok(())
    }
}

/// Serve one SOCKS5 connection: no authentication or a username (the connector pins a
/// circuit per chain with one), and `CONNECT` only.
fn socks(
    mut client: TcpStream,
    published: &Mutex<HashMap<(String, u16), SocketAddr>>,
) -> io::Result<()> {
    client.set_nonblocking(false)?;
    let mut head = [0u8; 2];
    client.read_exact(&mut head)?;
    if head[0] != 5 {
        return Ok(());
    }
    let mut methods = vec![0u8; head[1] as usize];
    client.read_exact(&mut methods)?;
    if methods.contains(&2) {
        client.write_all(&[5, 2])?;
        let mut version = [0u8; 2];
        client.read_exact(&mut version)?;
        let mut user = vec![0u8; version[1] as usize + 1];
        client.read_exact(&mut user)?;
        let mut password = vec![0u8; *user.last().unwrap_or(&0) as usize];
        client.read_exact(&mut password)?;
        client.write_all(&[1, 0])?;
    } else if methods.contains(&0) {
        client.write_all(&[5, 0])?;
    } else {
        return client.write_all(&[5, 0xff]);
    }

    let mut request = [0u8; 4];
    client.read_exact(&mut request)?;
    let host = match request[3] {
        1 => {
            let mut octets = [0u8; 4];
            client.read_exact(&mut octets)?;
            Ipv4Addr::from(octets).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            client.read_exact(&mut length)?;
            let mut name = vec![0u8; length[0] as usize];
            client.read_exact(&mut name)?;
            String::from_utf8_lossy(&name).to_ascii_lowercase()
        }
        _ => return client.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);

    let target = if request[1] != 1 {
        None
    } else if host.ends_with(&format!(".{TLD}")) {
        published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(host.clone(), port))
            .copied()
    } else if host == "localhost" || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback()) {
        Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    } else {
        None
    };
    let Some(target) = target else {
        // Not allowed by the ruleset: a stand-in goes nowhere that is not loopback.
        return client.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]);
    };
    let Ok(upstream) = TcpStream::connect_timeout(&target, Duration::from_secs(5)) else {
        return client.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
    };
    client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?;

    let (mut from_client, mut to_upstream) = (client.try_clone()?, upstream.try_clone()?);
    thread::spawn(move || {
        let _ = io::copy(&mut from_client, &mut to_upstream);
        let _ = to_upstream.shutdown(Shutdown::Write);
    });
    let (mut from_upstream, mut to_client) = (upstream, client);
    let _ = io::copy(&mut from_upstream, &mut to_client);
    let _ = to_client.shutdown(Shutdown::Write);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;

    /// Everything an edge promises, whichever edge it is. `fresh` makes another edge over
    /// the same agent node, as a restart does. `patience` is how long the overlay has to
    /// answer: a stand-in is quick and the real network is not.
    pub(crate) fn contract(
        edge: &dyn Edge,
        fresh: &dyn Fn() -> Box<dyn Edge>,
        dir: &Path,
        patience: Duration,
    ) {
        let key = |name: &str| {
            let path = dir.join(name);
            // Fresh keys each run: the real network remembers an address it has seen.
            let secret = crate::keystore::random::<32>().unwrap();
            fs::write(&path, secret).unwrap();
            path
        };
        let (first, second) = (key("a"), key("b"));

        // An address per connector, in the overlay's own domain, the same every time.
        let one = edge.issue(0, &first).unwrap();
        assert!(one.ends_with(".anyone"), "{one}");
        assert_eq!(56 + ".anyone".len(), one.len());
        assert_eq!(one, edge.issue(0, &first).unwrap());
        assert_ne!(one, edge.issue(1, &second).unwrap());
        // ... and after a restart.
        assert_eq!(one, fresh().issue(0, &first).unwrap());

        // A proxy that gets to what is published behind an address, at each port.
        let connector = serve("connector");
        let relay = serve("relay");
        edge.publish(
            &one,
            &[(CONNECTOR_PORT, connector), (RELAY_READ_PORT, relay)],
        )
        .unwrap();
        assert_eq!(
            "connector",
            reaches(edge.proxy(), &one, CONNECTOR_PORT, patience)
        );
        assert_eq!(
            "relay",
            reaches(edge.proxy(), &one, RELAY_READ_PORT, patience)
        );
        // An address nothing is published at is not reached.
        assert!(through(edge.proxy(), &one, 81, patience).is_none());
        let other = edge.issue(1, &second).unwrap();
        assert!(through(edge.proxy(), &other, CONNECTOR_PORT, patience).is_none());
    }

    /// A server that answers one line, `name`, to whoever connects.
    fn serve(name: &'static str) -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = writeln!(stream, "{name}");
            }
        });
        address
    }

    /// `through`, again until it answers: a published endpoint takes a while to be found.
    fn reaches(proxy: SocketAddr, host: &str, port: u16, patience: Duration) -> String {
        let deadline = std::time::Instant::now() + patience;
        loop {
            if let Some(line) = through(proxy, host, port, patience) {
                return line;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{host}:{port} was not reached in {patience:?}"
            );
            thread::sleep(Duration::from_secs(2));
        }
    }

    /// Ask the proxy for `host:port` by name, as `socks5h` does, and read the line it says.
    fn through(proxy: SocketAddr, host: &str, port: u16, patience: Duration) -> Option<String> {
        let mut stream = TcpStream::connect(proxy).unwrap();
        stream.set_read_timeout(Some(patience)).unwrap();
        stream.write_all(&[5, 1, 0]).unwrap();
        let mut answer = [0u8; 2];
        stream.read_exact(&mut answer).unwrap();
        assert_eq!([5, 0], answer);
        let mut request = vec![5, 1, 0, 3, host.len() as u8];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).unwrap();
        if reply[1] != 0 {
            return None;
        }
        let mut line = String::new();
        io::BufReader::new(stream).read_line(&mut line).ok()?;
        Some(line.trim().to_owned())
    }

    #[test]
    fn the_loopback_stand_in_meets_the_contract() {
        let home = tempfile::tempdir().unwrap();
        let edge = Loopback::start(home.path()).unwrap();
        let path = home.path().to_path_buf();
        contract(
            &edge,
            &move || Box::new(Loopback::start(&path).unwrap()),
            home.path(),
            Duration::from_secs(5),
        );
        // The address is kept where the daemon keeps its own.
        let kept = fs::read_to_string(home.path().join("overlay/0/hostname")).unwrap();
        assert!(kept.trim().ends_with(".anyone"));
    }

    /// What the contract says of an edge that is found by the next process: the second
    /// proxy is the first's, and what the first published is reached through it.
    #[test]
    fn the_stand_in_is_found_by_the_next_process_while_it_answers() {
        let home = tempfile::tempdir().unwrap();
        let first = Loopback::start(home.path()).unwrap();
        let connector = serve("connector");
        let key = home.path().join("key");
        fs::write(&key, [9u8; 32]).unwrap();
        let address = first.issue(0, &key).unwrap();
        first
            .publish(&address, &[(CONNECTOR_PORT, connector)])
            .unwrap();

        // Another process does not share this one's memory: it reads the file the first left.
        STARTED
            .lock()
            .unwrap()
            .retain(|(dir, _)| !dir.starts_with(home.path()));
        let second = Loopback::start(home.path()).unwrap();

        assert_eq!(first.proxy(), second.proxy());
        assert_eq!(
            "connector",
            through(
                second.proxy(),
                &address,
                CONNECTOR_PORT,
                Duration::from_secs(5)
            )
            .unwrap()
        );
        let file = home.path().join("overlay/loopback");
        assert!(file.exists());
        drop(second);
        assert!(file.exists(), "the one that found it does not take it away");
        drop(first);
        assert!(!file.exists());

        // With none answering, a new one is started.
        let third = Loopback::start(home.path()).unwrap();
        assert!(answers(third.proxy()));
    }

    #[test]
    fn a_declared_name_stands_for_a_loopback_address() {
        let home = tempfile::tempdir().unwrap();
        let server = serve("declared");
        let other = serve("elsewhere");
        let names = format!(
            "relay.anyone:7100={server}, bad.anyone:1=8.8.8.8:80,{}:81={other}",
            "Mixed.anyone"
        );
        let edge = Loopback::start_naming(home.path(), &names).unwrap();
        let patience = Duration::from_secs(5);

        assert_eq!(
            Some("declared".into()),
            through(edge.proxy(), "relay.anyone", 7100, patience)
        );
        assert_eq!(
            Some("elsewhere".into()),
            through(edge.proxy(), "mixed.anyone", 81, patience)
        );
        // Another port, and an address that is not loopback, are not declared.
        assert!(through(edge.proxy(), "relay.anyone", 7101, patience).is_none());
        assert!(through(edge.proxy(), "bad.anyone", 1, patience).is_none());
    }

    #[test]
    fn the_stand_in_passes_loopback_through_and_refuses_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let edge = Loopback::start(home.path()).unwrap();
        let server = serve("here");
        assert_eq!(
            Some("here".into()),
            through(
                edge.proxy(),
                "127.0.0.1",
                server.port(),
                Duration::from_secs(5)
            )
        );
        assert!(through(edge.proxy(), "example.com", 443, Duration::from_secs(5)).is_none());
    }

    #[test]
    fn an_onion_address_is_the_checksummed_key_in_base32() {
        // The version byte, 3, is the last five bits of the address: `d`.
        let address = address_of(&[7; 32]);
        assert_eq!(address, address_of(&[7; 32]));
        assert_ne!(address, address_of(&[8; 32]));
        assert!(address
            .trim_end_matches(".anyone")
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)));
        assert!(address.trim_end_matches(".anyone").ends_with('d'));
    }
}
