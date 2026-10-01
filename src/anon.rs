//! The real overlay edge: the `anon` daemon (ADR 0003).
//!
//! The supervisor downloads Anyone's `anon` release, checks it against a checksum pinned
//! here, and runs one daemon for the machine. Every connector is a hidden service of that
//! daemon, with the key the wallet derives for it, so each has its own `.anyone` address
//! and the address does not change when the daemon does. The daemon is the machine's, not
//! a process's: it is detached, and the next `toon` to need an overlay finds it by the
//! state it left in `overlay/` and uses it.
//!
//! It fails closed. A release that cannot be downloaded or does not match its checksum is
//! never run, a daemon that does not bootstrap is killed, and neither falls back to
//! anything: the caller gets `overlay_unavailable`. The daemon is only started once the
//! operator has agreed to Anyone's terms, which is recorded in `overlay/agreed`.
//!
//! Layout of `overlay/`, beside the files the loopback stand-in keeps:
//!
//! - `bin/<version>/`: the verified release zip, and what was extracted from it
//! - `anonrc`, `anon.log`, `data/`: the daemon's configuration, log and state
//! - `daemon`: the daemon's pid and SOCKS port, while it runs
//! - `services.d/<n>.conf`: the hidden service of connector `n`, included by `anonrc`
//! - `<n>/`: connector `n`'s hidden-service directory, with its key and its `hostname`

use std::fs;
use std::io::{self, Cursor, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256, Sha512};

use crate::outcome::{Error, ErrorCode};
use crate::overlay::{self, Edge};

/// The release of `anon` that is run. Moving it means changing the checksums with it.
pub const VERSION: &str = "0.4.10.2";

/// The pinned releases: operating system, architecture as the release names it, and the
/// SHA-256 of `anon-live-<os>-<arch>.zip`.
const RELEASES: [(&str, &str, &str); 4] = [
    (
        "linux-amd64",
        "x86_64",
        "9c6498b8d27de54d78842a1b854979a605f9c140ccc34f2b4c267bf094eaeb17",
    ),
    (
        "linux-arm64",
        "aarch64",
        "daa3ed15f321d83f22b9c03fad2c0f908e60e3e156d143774bd38194244ebec9",
    ),
    (
        "macos-amd64",
        "x86_64",
        "38b5b7ce1ed847e791e56697385365918fbcf81d08def0d389abd8e31e58b975",
    ),
    (
        "macos-arm64",
        "aarch64",
        "8f662cc5ecba27b5ec07dbed897894bd6f0e6c5ff33ca01f3a1d19544ac67866",
    ),
];

/// Replaces where the release is downloaded from, for tests. The checksum still applies.
pub const MIRROR_VARIABLE: &str = "TOON_ANON_MIRROR";

/// How long the daemon has to bootstrap before it is given up on.
const BOOTSTRAP: Duration = Duration::from_secs(180);

/// How long a hidden service has to be written out by the daemon after a reload.
const HOSTNAME: Duration = Duration::from_secs(60);

/// Where an unpublished hidden service sends its connections: nowhere that listens.
const NOWHERE: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 9);

/// The pinned release for this machine: its asset name and its checksum.
fn release(os: &str, arch: &str) -> Option<(String, &'static str)> {
    let os = match os {
        "linux" | "macos" => os,
        _ => return None,
    };
    RELEASES
        .iter()
        .find(|(name, machine, _)| name.starts_with(os) && *machine == arch)
        .map(|(name, _, sha)| (format!("anon-live-{name}.zip"), *sha))
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

fn io_error(path: &Path, source: io::Error) -> Error {
    unavailable(&format!("{}: {source}", path.display()))
}

/// The directory of `home`'s overlay.
pub fn directory(home: &Path) -> PathBuf {
    home.join("overlay")
}

/// Bring up the overlay of `home`: use the daemon that is running, or download, verify and
/// start one. `agreed` is the operator's agreement to Anyone's terms given just now; an
/// agreement given before, when the daemon was first started, is recorded and is enough.
/// What this created is removed again if it fails.
pub fn bootstrap(home: &Path, agreed: bool) -> Result<Anon, Error> {
    let dir = directory(home);
    let existed = dir.exists();
    let anon = start(&dir, agreed);
    if anon.is_err() && !existed {
        let _ = fs::remove_dir_all(&dir);
    }
    anon
}

fn start(dir: &Path, agreed: bool) -> Result<Anon, Error> {
    let recorded = dir.join("agreed");
    if !agreed && !recorded.exists() {
        return Err(unavailable(
            "the operator has not agreed to the Anyone Protocol's terms, which the daemon \
             needs: create the TOON app with `--accept-anyone-terms`",
        ));
    }

    let socks = match running(dir) {
        Some(socks) => {
            wait_bootstrapped(dir, None)?;
            socks
        }
        None => {
            let binary = install(dir)?;
            let (mut child, socks) = spawn(dir, &binary)?;
            if let Err(error) = wait_bootstrapped(dir, Some(&mut child)) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(dir.join("daemon"));
                return Err(error);
            }
            socks
        }
    };
    if !recorded.exists() {
        crate::node::write(&recorded, b"agreed\n", 0o600)?;
    }
    Ok(Anon {
        dir: dir.to_path_buf(),
        socks,
        issued: Mutex::new(Vec::new()),
    })
}

/// The SOCKS port of the daemon that runs, if one does: it has left its state, its process
/// is there and its proxy answers.
fn running(dir: &Path) -> Option<SocketAddr> {
    let state = fs::read_to_string(dir.join("daemon")).ok()?;
    let mut fields = state.split_whitespace();
    let pid = fields.next()?;
    let socks = SocketAddr::from((Ipv4Addr::LOCALHOST, fields.next()?.parse().ok()?));
    let alive = signal(pid, "0");
    let answers = TcpStream::connect_timeout(&socks, Duration::from_secs(2)).is_ok();
    if alive && answers {
        return Some(socks);
    }
    let _ = fs::remove_file(dir.join("daemon"));
    None
}

fn signal(pid: &str, signal: &str) -> bool {
    Command::new("kill")
        .args([&format!("-{signal}"), pid])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Stop the machine's daemon, if one runs.
pub fn stop(home: &Path) {
    let dir = directory(home);
    if let Ok(state) = fs::read_to_string(dir.join("daemon")) {
        if let Some(pid) = state.split_whitespace().next() {
            signal(pid, "TERM");
        }
    }
    let _ = fs::remove_file(dir.join("daemon"));
}

/// The `anon` binary of the pinned release, downloaded and checked if it is not there.
/// What is run was extracted from a zip whose checksum matched, which is checked again
/// every time.
fn install(dir: &Path) -> Result<PathBuf, Error> {
    let machine = (std::env::consts::OS, std::env::consts::ARCH);
    let (asset, sha) = release(machine.0, machine.1).ok_or_else(|| {
        unavailable(&format!(
            "there is no `anon` release for {} on {}",
            machine.1, machine.0
        ))
    })?;
    let target = dir.join("bin").join(VERSION);
    let zip = target.join("release.zip");

    let bytes = match fs::read(&zip) {
        Ok(bytes) if checksum(&bytes) == sha => bytes,
        _ => {
            let bytes = download(&asset)?;
            verify(&bytes, sha)?;
            crate::node::write(&zip, &bytes, 0o600)?;
            bytes
        }
    };
    extract(&bytes, &target)?;
    Ok(target.join("anon"))
}

fn checksum(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn verify(bytes: &[u8], sha: &str) -> Result<(), Error> {
    let found = checksum(bytes);
    if found == sha {
        return Ok(());
    }
    Err(unavailable(&format!(
        "the downloaded `anon` release has the checksum {found}, not the pinned {sha}, so it \
         was not run"
    )))
}

fn download(asset: &str) -> Result<Vec<u8>, Error> {
    let base = std::env::var(MIRROR_VARIABLE)
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| {
            format!("https://github.com/anyone-protocol/ator-protocol/releases/download/v{VERSION}")
        });
    let url = format!("{}/{asset}", base.trim_end_matches('/'));
    let failed =
        |why: &dyn std::fmt::Display| unavailable(&format!("{url} could not be downloaded: {why}"));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|error| failed(&error))?;
    let response = client
        .get(&url)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| failed(&error))?;
    Ok(response.bytes().map_err(|error| failed(&error))?.to_vec())
}

/// Extract the daemon and its geoip files from the release into `target`.
fn extract(bytes: &[u8], target: &Path) -> Result<(), Error> {
    let corrupt = |why: &dyn std::fmt::Display| {
        unavailable(&format!("the `anon` release is not a zip: {why}"))
    };
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| corrupt(&error))?;
    for name in ["anon", "geoip", "geoip6"] {
        let mut file = archive
            .by_name(name)
            .map_err(|_| unavailable(&format!("the `anon` release has no `{name}`")))?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|error| corrupt(&error))?;
        let mode = if name == "anon" { 0o700 } else { 0o600 };
        let path = target.join(name);
        // A binary that is being run cannot be overwritten in place.
        let _ = fs::remove_file(&path);
        crate::node::write(&path, &contents, mode)?;
    }
    Ok(())
}

/// Write the daemon's configuration and start it, detached from this process.
fn spawn(dir: &Path, binary: &Path) -> Result<(Child, SocketAddr), Error> {
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map_err(|error| unavailable(&format!("no port for the proxy: {error}")))?
        .port();
    let services = dir.join("services.d");
    fs::create_dir_all(&services).map_err(|error| io_error(&services, error))?;
    let log = dir.join("anon.log");
    let _ = fs::remove_file(&log);
    let config = dir.join("anonrc");
    crate::node::write(&config, anonrc(dir, binary, port).as_bytes(), 0o600)?;

    let child = Command::new(binary)
        .arg("-f")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so that it outlives the command that started it.
        .process_group(0)
        .spawn()
        .map_err(|error| io_error(binary, error))?;
    crate::node::write(
        &dir.join("daemon"),
        format!("{} {port}\n", child.id()).as_bytes(),
        0o600,
    )?;
    Ok((child, SocketAddr::from((Ipv4Addr::LOCALHOST, port))))
}

/// The daemon's configuration. It listens for SOCKS on loopback only, has no control
/// port, and takes its hidden services from `services.d`.
fn anonrc(dir: &Path, binary: &Path, port: u16) -> String {
    let beside = binary.parent().unwrap_or(dir);
    format!(
        "AgreeToTerms 1\n\
         DataDirectory {data}\n\
         SocksPort 127.0.0.1:{port}\n\
         ControlPort 0\n\
         Log notice file {log}\n\
         GeoIPFile {geoip}\n\
         GeoIPv6File {geoip6}\n\
         %include {services}\n",
        data = dir.join("data").display(),
        log = dir.join("anon.log").display(),
        geoip = beside.join("geoip").display(),
        geoip6 = beside.join("geoip6").display(),
        services = dir.join("services.d").display(),
    )
}

/// Whether the daemon's log says it has finished bootstrapping.
fn bootstrapped(log: &str) -> bool {
    log.lines()
        .rev()
        .find(|line| line.contains("Bootstrapped "))
        .is_some_and(|line| line.contains("Bootstrapped 100%"))
}

fn wait_bootstrapped(dir: &Path, mut child: Option<&mut Child>) -> Result<(), Error> {
    let log = dir.join("anon.log");
    let deadline = Instant::now() + BOOTSTRAP;
    loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        if bootstrapped(&text) {
            return Ok(());
        }
        if let Some(Ok(Some(status))) = child.as_mut().map(|child| child.try_wait()) {
            return Err(unavailable(&format!(
                "the `anon` daemon exited with {status}: {}",
                last_lines(&text)
            )));
        }
        if Instant::now() >= deadline {
            return Err(unavailable(&format!(
                "the `anon` daemon did not bootstrap in {} seconds: {}",
                BOOTSTRAP.as_secs(),
                last_lines(&text)
            )));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn last_lines(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(2)..].join(" / ")
}

/// The file `anon` keeps a hidden service's key in: a header, then the 64-byte expanded
/// form of the Ed25519 seed the wallet derived, which is the key `address_of` names.
fn key_file(seed: &[u8; 32]) -> Vec<u8> {
    let mut expanded: [u8; 64] = Sha512::digest(seed).into();
    expanded[0] &= 248;
    expanded[31] &= 127;
    expanded[31] |= 64;
    let mut file = b"== ed25519v1-secret: type0 ==\0\0\0".to_vec();
    file.extend_from_slice(&expanded);
    file
}

/// The daemon, from this process's side.
pub struct Anon {
    dir: PathBuf,
    socks: SocketAddr,
    /// The connector each address was issued to, in this process.
    issued: Mutex<Vec<(String, u32)>>,
}

impl Anon {
    fn service(&self, connector: u32) -> PathBuf {
        self.dir
            .join("services.d")
            .join(format!("{connector}.conf"))
    }

    /// Write connector `connector`'s hidden service, serving `ports`, and have the daemon
    /// reload it if that changed anything.
    fn write_service(&self, connector: u32, ports: &[(u16, SocketAddr)]) -> Result<bool, Error> {
        let mut config = format!(
            "HiddenServiceDir {}\nHiddenServiceVersion 3\n",
            self.dir.join(connector.to_string()).display()
        );
        for (port, target) in ports {
            config.push_str(&format!("HiddenServicePort {port} {target}\n"));
        }
        let path = self.service(connector);
        if fs::read_to_string(&path).is_ok_and(|kept| kept == config) {
            return Ok(false);
        }
        crate::node::write(&path, config.as_bytes(), 0o600)?;
        let state = fs::read_to_string(self.dir.join("daemon")).unwrap_or_default();
        let pid = state.split_whitespace().next().unwrap_or_default();
        if !signal(pid, "HUP") {
            return Err(unavailable("the `anon` daemon is no longer running"));
        }
        Ok(true)
    }
}

impl Edge for Anon {
    fn proxy(&self) -> SocketAddr {
        self.socks
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
        let address = overlay::address_of(&secret);

        let service = self.dir.join(connector.to_string());
        let secret_file = service.join("hs_ed25519_secret_key");
        let expected = zeroize::Zeroizing::new(key_file(&secret));
        if fs::read(&secret_file).ok().as_deref() != Some(expected.as_slice()) {
            // Whatever the daemon made for another key is not this connector's.
            let _ = fs::remove_dir_all(&service);
            crate::node::write(&secret_file, &expected, 0o600)?;
        }
        fs::set_permissions(&service, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error(&service, error))?;

        // Until something is published, the endpoint is up and goes nowhere.
        let reloaded = self.write_service(connector, &[(overlay::CONNECTOR_PORT, NOWHERE)])?;
        let hostname = service.join("hostname");
        let deadline = Instant::now() + HOSTNAME;
        let named = loop {
            match fs::read_to_string(&hostname) {
                Ok(name) if !name.trim().is_empty() => break name.trim().to_owned(),
                _ if Instant::now() >= deadline => {
                    return Err(unavailable(&format!(
                        "the daemon did not write {}{}",
                        hostname.display(),
                        if reloaded {
                            ""
                        } else {
                            " for a service it has"
                        }
                    )))
                }
                _ => thread::sleep(Duration::from_millis(250)),
            }
        };
        if named != address {
            return Err(unavailable(&format!(
                "the daemon named connector {connector} {named}, and its key makes {address}"
            )));
        }
        let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
        issued.retain(|(known, _)| *known != address);
        issued.push((address.clone(), connector));
        Ok(address)
    }

    fn publish(&self, address: &str, ports: &[(u16, SocketAddr)]) {
        let connector = {
            let issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            issued
                .iter()
                .find(|(known, _)| known == address)
                .map(|(_, n)| *n)
        };
        if let Some(connector) = connector {
            let _ = self.write_service(connector, ports);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_is_pinned_for_each_platform_that_has_one() {
        let (asset, sha) = release("linux", "x86_64").unwrap();
        assert_eq!("anon-live-linux-amd64.zip", asset);
        assert_eq!(64, sha.len());
        assert_eq!(
            "anon-live-linux-arm64.zip",
            release("linux", "aarch64").unwrap().0
        );
        assert_eq!(
            "anon-live-macos-arm64.zip",
            release("macos", "aarch64").unwrap().0
        );
        assert_eq!(
            "anon-live-macos-amd64.zip",
            release("macos", "x86_64").unwrap().0
        );
        assert!(release("windows", "x86_64").is_none());
        assert!(release("linux", "riscv64").is_none());
    }

    #[test]
    fn a_download_that_does_not_match_its_checksum_is_refused() {
        let sha = checksum(b"the release");
        assert!(verify(b"the release", &sha).is_ok());
        let error = verify(b"another release", &sha).unwrap_err();
        assert_eq!(ErrorCode::OverlayUnavailable, error.code);
        assert!(error.message.contains("not run"), "{}", error.message);
    }

    #[test]
    fn the_key_file_is_a_header_and_the_clamped_expanded_seed() {
        let file = key_file(&[7; 32]);
        assert_eq!(96, file.len());
        assert_eq!(&b"== ed25519v1-secret: type0 =="[..], &file[..29]);
        assert_eq!(0, file[0x20] & 7);
        assert_eq!(0x40, file[0x20 + 31] & 0xc0);
    }

    #[test]
    fn the_daemon_is_bootstrapped_when_its_last_progress_line_says_so() {
        assert!(!bootstrapped(""));
        assert!(!bootstrapped("[notice] Bootstrapped 55% (loading)\n"));
        assert!(bootstrapped(
            "[notice] Bootstrapped 95% (x)\n[notice] Bootstrapped 100% (done): Done\n"
        ));
    }

    #[test]
    fn the_daemon_listens_on_loopback_and_takes_its_services_from_a_directory() {
        let config = anonrc(Path::new("/o"), Path::new("/o/bin/anon"), 9050);
        assert!(config.contains("AgreeToTerms 1\n"));
        assert!(config.contains("SocksPort 127.0.0.1:9050\n"));
        assert!(config.contains("ControlPort 0\n"));
        assert!(config.contains("%include /o/services.d\n"));
    }

    /// Run by hand against the real network: `cargo test anon_meets_the_contract --
    /// --ignored --nocapture`. It downloads the release, bootstraps and publishes.
    #[test]
    #[ignore = "needs the Anyone network"]
    fn anon_meets_the_contract() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_path_buf();
        let edge = bootstrap(home.path(), true).unwrap();
        overlay::tests::contract(
            &edge,
            &move || Box::new(bootstrap(&path, false).unwrap()),
            home.path(),
            Duration::from_secs(240),
        );
        stop(home.path());
    }
}
