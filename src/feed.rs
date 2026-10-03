//! The receiving end of a live feed: dial a relay, prove which key we hold with NIP-42,
//! send one `REQ` and hand on each event (`nips/paid-subscription.md`, "The live feed").
//!
//! The supervisor (`receive`) reads other relays' feeds as the subscriber, and `toon event
//! watch` reads the agent node's own relay as its operator. A relay
//! reached through the overlay is dialled through its SOCKS5 proxy, naming the host
//! (`socks5h`), so that nothing is resolved on this machine; a `wss://` relay's TLS runs
//! inside the stream the proxy returns.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::event;
use crate::relay_url::RelayUrl;
use crate::tls;

/// How long a relay gets to answer the connection: its `AUTH` challenge and the proof.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a read gives up waiting, so that a feed that is asked to stop notices.
const TICK: Duration = Duration::from_millis(250);

/// NIP-42: a kind of event that answers an `AUTH` challenge.
const CLIENT_AUTH: u64 = 22242;

/// How long a read of the agent node's own relay waits for an `AUTH` challenge before it
/// asks for events without answering one: a relay that does not sell its feed need not
/// challenge.
const CHALLENGE_GRACE: Duration = Duration::from_secs(1);

/// The id of the one `REQ` a feed sends.
pub const SUBSCRIPTION: &str = "feed";

/// How a feed ended.
#[derive(Debug, PartialEq)]
pub enum Ended {
    /// The relay closed the `REQ` with `payment-required`: the balance has run out, or
    /// this connection holds no subscription.
    Exhausted(String),
    /// The relay closed the `REQ` for another reason.
    Closed(String),
    /// The connection could not be made or dropped.
    Dropped(String),
    /// The caller asked it to stop.
    Stopped,
}

/// Connect to `host:port` through the SOCKS5 proxy at `proxy`, which resolves the name.
fn through(proxy: SocketAddr, host: &str, port: u16) -> Result<TcpStream, String> {
    let refused = |what: &str| format!("The overlay's proxy {what}.");
    let mut stream = TcpStream::connect_timeout(&proxy, PATIENCE).map_err(|error| {
        format!("The overlay's proxy at {proxy} did not accept a connection: {error}.")
    })?;
    stream.set_read_timeout(Some(PATIENCE)).ok();
    stream.set_write_timeout(Some(PATIENCE)).ok();
    let io = |error: std::io::Error| format!("The overlay's proxy stopped answering: {error}.");
    stream.write_all(&[5, 1, 0]).map_err(io)?;
    let mut method = [0u8; 2];
    stream.read_exact(&mut method).map_err(io)?;
    if method != [5, 0] {
        return Err(refused("wants an authentication that is not offered"));
    }
    let name = host.as_bytes();
    let length = u8::try_from(name.len()).map_err(|_| format!("{host} is too long a name."))?;
    let mut request = vec![5, 1, 0, 3, length];
    request.extend_from_slice(name);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).map_err(io)?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).map_err(io)?;
    if head[1] != 0 {
        return Err(format!(
            "The overlay's proxy could not reach {host}:{port} (code {}).",
            head[1]
        ));
    }
    let rest = match head[3] {
        1 => 4 + 2,
        4 => 16 + 2,
        3 => {
            let mut length = [0u8; 1];
            stream.read_exact(&mut length).map_err(io)?;
            usize::from(length[0]) + 2
        }
        _ => return Err(refused("answered in a form that is not understood")),
    };
    let mut ignored = vec![0u8; rest];
    stream.read_exact(&mut ignored).map_err(io)?;
    Ok(stream)
}

/// Connect to `host:port`, giving each address it resolves to the patience.
fn direct(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = None;
    for address in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, PATIENCE) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("the name resolves to no address")))
}

/// A websocket to a relay, in TLS when the relay is `wss://`.
pub type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// The connection under a socket, whether it is wrapped in TLS or not.
pub fn tcp(socket: &Socket) -> &TcpStream {
    match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_ref(),
        _ => unreachable!("only rustls is compiled in"),
    }
}

/// How long a read on `socket` waits before it gives up, and how long a write does.
pub fn set_timeouts(socket: &Socket, read: Duration, write: Duration) -> std::io::Result<()> {
    let stream = tcp(socket);
    stream.set_read_timeout(Some(read))?;
    stream.set_write_timeout(Some(write))
}

/// Dial `relay` and complete the websocket handshake, after the TLS one if it is `wss://`.
/// Through `proxy` the proxy is asked for the host by name and the TLS runs inside the
/// stream it returns.
pub fn dial(relay: &str, proxy: Option<SocketAddr>) -> Result<Socket, String> {
    let url = RelayUrl::parse(relay)?;
    let connector = if url.tls {
        Some(tls::connector()?)
    } else {
        None
    };
    let stream = match proxy {
        Some(proxy) => through(proxy, &url.host, url.port)?,
        None => direct(&url.host, url.port)
            .map_err(|error| format!("{relay} did not accept a connection: {error}."))?,
    };
    // The handshakes get the patience; once they are done a read gives up every tick.
    stream.set_read_timeout(Some(PATIENCE)).ok();
    stream.set_write_timeout(Some(PATIENCE)).ok();
    let (socket, _) =
        tungstenite::client_tls_with_config(relay, stream, None, connector).map_err(|error| {
            let certificate = matches!(&error,
                tungstenite::HandshakeError::Failure(failure) if tls::is_certificate(failure));
            let error = tls::chain(&error);
            if certificate {
                format!("The certificate of {relay} did not verify: {error}.")
            } else {
                format!("{relay} did not complete a websocket handshake: {error}.")
            }
        })?;
    tcp(&socket).set_read_timeout(Some(TICK)).ok();
    Ok(socket)
}

/// Whether a read gave up waiting, which is not an error.
fn timed_out(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(source)
        if matches!(source.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

/// The frame of a text message, if it is a JSON array.
fn frame(message: Message) -> Option<Vec<Value>> {
    let Message::Text(text) = message else {
        return None;
    };
    match serde_json::from_str(&text) {
        Ok(Value::Array(frame)) => Some(frame),
        _ => None,
    }
}

/// The NIP-42 answer of the key `secret` to the challenge `challenge` of the relay reached
/// at `url`.
fn answer(secret: &[u8; 32], url: &str, challenge: &str) -> Result<Value, Ended> {
    event::sign(
        secret,
        event::now(),
        CLIENT_AUTH,
        json!([["relay", url], ["challenge", challenge]]),
        "",
    )
    .map_err(|error| Ended::Dropped(error.message))
}

/// How a feed is read: whom it answers an `AUTH` challenge as, and what it keeps.
pub struct Reading<'a> {
    /// The key that answers the relay's `AUTH` challenge, and the URL the `relay` tag of the
    /// answer names, which is where the relay is reached and not always the address that
    /// is dialled. None sends the `REQ` at once, for a relay that sends no challenge.
    pub login: Option<(&'a [u8; 32], &'a str)>,
    /// Hand on only the events that arrive after the relay's `EOSE`, and not the stored
    /// ones it sends first.
    pub live_only: bool,
}

/// Read the live feed of `relay` for the subscriber key `secret`, with `filter`, and call
/// `on_event` for every event it sends, until it ends. `on_event` returns whether to go on.
pub fn read(
    relay: &str,
    secret: &[u8; 32],
    filter: &Value,
    proxy: Option<SocketAddr>,
    stop: &AtomicBool,
    on_event: impl FnMut(Value) -> bool,
) -> Ended {
    let reading = Reading {
        login: Some((secret, relay)),
        live_only: false,
    };
    listen(relay, &reading, filter, proxy, stop, on_event)
}

/// Read the live feed of `relay` as `reading` says, until it ends.
pub fn listen(
    relay: &str,
    reading: &Reading,
    filter: &Value,
    proxy: Option<SocketAddr>,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Value) -> bool,
) -> Ended {
    let mut socket = match dial(relay, proxy) {
        Ok(socket) => socket,
        Err(message) => return Ended::Dropped(message),
    };
    let dropped = |error: tungstenite::Error| Ended::Dropped(format!("{relay}: {error}."));

    let mut opening = Vec::new();
    if let Some((secret, url)) = reading.login {
        // The relay opens with an `AUTH` challenge; the feed is for the key that answers it.
        let started = Instant::now();
        let challenge = loop {
            if stop.load(Ordering::SeqCst) {
                return Ended::Stopped;
            }
            if started.elapsed() > PATIENCE {
                return Ended::Dropped(format!("{relay} sent no AUTH challenge."));
            }
            match socket.read() {
                Ok(message) => {
                    if let Some(frame) = frame(message) {
                        if frame.first().and_then(Value::as_str) == Some("AUTH") {
                            if let Some(challenge) = frame.get(1).and_then(Value::as_str) {
                                break challenge.to_owned();
                            }
                        }
                    }
                }
                Err(error) if timed_out(&error) => {}
                Err(error) => return dropped(error),
            }
        };
        let answer = match answer(secret, url, &challenge) {
            Ok(answer) => answer,
            Err(ended) => return ended,
        };
        opening.push(json!(["AUTH", answer]));
    }
    let mut filter = filter.clone();
    if let Some(filter) = filter.as_object_mut() {
        // A filter's `limit` has no meaning for a live feed.
        filter.remove("limit");
    }
    opening.push(json!(["REQ", SUBSCRIPTION, filter]));
    for message in opening {
        if let Err(error) = socket.send(Message::text(message.to_string())) {
            return dropped(error);
        }
    }

    let mut stored = reading.live_only;

    let ended = loop {
        if stop.load(Ordering::SeqCst) {
            break Ended::Stopped;
        }
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if timed_out(&error) => continue,
            Err(error) => break dropped(error),
        };
        let Some(frame) = frame(message) else {
            continue;
        };
        match (frame.first().and_then(Value::as_str), frame.get(1)) {
            (Some("EOSE"), Some(id)) if id == SUBSCRIPTION => stored = false,
            (Some("EVENT"), Some(id)) if id == SUBSCRIPTION => {
                if stored {
                    continue;
                }
                if let Some(event) = frame.get(2) {
                    if !on_event(event.clone()) {
                        break Ended::Stopped;
                    }
                }
            }
            (Some("CLOSED"), Some(id)) if id == SUBSCRIPTION => {
                let reason = frame.get(2).and_then(Value::as_str).unwrap_or_default();
                // A client branches on the prefix and not on the text after it.
                break if reason.starts_with("payment-required:") {
                    Ended::Exhausted(reason.to_owned())
                } else {
                    Ended::Closed(reason.to_owned())
                };
            }
            _ => {}
        }
    };
    let _ = socket.close(None);
    ended
}

/// Read the live feed of the agent node's own `relay` with `filter` and call `on_event` for
/// every event it sends, until it ends. A relay that sells its feed closes a free read after
/// the stored events but keeps one open for a connection that proves an operator key, and
/// the relay's own identity key, `identity_key`, is always one: so a challenge is answered
/// with it, naming `url`, where the relay is reached, which is not always the address that
/// is dialled, and the `REQ` is sent once the challenge is answered. A relay that sends no
/// challenge within a moment is asked without one.
pub fn read_own(
    relay: &str,
    url: &str,
    identity_key: &[u8; 32],
    filter: &Value,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Value),
) -> Ended {
    let mut socket = match dial(relay, None) {
        Ok(socket) => socket,
        Err(message) => return Ended::Dropped(message),
    };
    let dropped = |error: tungstenite::Error| Ended::Dropped(format!("{relay}: {error}."));
    let request = json!(["REQ", SUBSCRIPTION, filter]).to_string();
    let started = Instant::now();
    let (mut requested, mut answered) = (false, false);
    let ended = loop {
        if stop.load(Ordering::SeqCst) {
            break Ended::Stopped;
        }
        if !requested && started.elapsed() > CHALLENGE_GRACE {
            if let Err(error) = socket.send(Message::text(request.clone())) {
                break dropped(error);
            }
            requested = true;
        }
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) if timed_out(&error) => continue,
            Err(error) => break dropped(error),
        };
        let Some(frame) = frame(message) else {
            continue;
        };
        match (frame.first().and_then(Value::as_str), frame.get(1)) {
            (Some("AUTH"), Some(challenge)) if !answered => {
                let Some(challenge) = challenge.as_str() else {
                    continue;
                };
                let answer = match answer(identity_key, url, challenge) {
                    Ok(answer) => answer,
                    Err(ended) => break ended,
                };
                answered = true;
                // A `REQ` sent before the answer is asked again, as the relay may have
                // read it as a free one.
                for message in [json!(["AUTH", answer]).to_string(), request.clone()] {
                    if let Err(error) = socket.send(Message::text(message)) {
                        return dropped(error);
                    }
                }
                requested = true;
            }
            (Some("EVENT"), Some(id)) if id == SUBSCRIPTION => {
                if let Some(event) = frame.get(2) {
                    on_event(event.clone());
                }
            }
            (Some("CLOSED"), Some(id)) if id == SUBSCRIPTION => {
                let reason = frame.get(2).and_then(Value::as_str).unwrap_or_default();
                break Ended::Closed(reason.to_owned());
            }
            _ => {}
        }
    };
    let _ = socket.close(None);
    ended
}
