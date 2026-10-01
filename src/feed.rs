//! The receiving end of a subscription: dial another relay's live feed, prove which
//! subscriber key we hold with NIP-42, send one `REQ` and hand on each event
//! (`nips/paid-subscription.md`, "The live feed").
//!
//! The supervisor (`receive`) and `toon event follow` both read a feed this way. A relay
//! reached through the overlay is dialled through its SOCKS5 proxy, naming the host
//! (`socks5h`), so that nothing is resolved on this machine.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

use crate::event;

/// How long a relay gets to answer the connection: its `AUTH` challenge and the proof.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a read gives up waiting, so that a feed that is asked to stop notices.
const TICK: Duration = Duration::from_millis(250);

/// NIP-42: a kind of event that answers an `AUTH` challenge.
const CLIENT_AUTH: u64 = 22242;

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

/// `host` and `port` of a `ws://host:port` URL.
fn authority(relay: &str) -> Result<(String, u16), String> {
    let rest = relay.strip_prefix("ws://").ok_or_else(|| {
        format!("{relay} is not a ws:// URL; this build dials plain websocket relays only.")
    })?;
    let rest = rest.split('/').next().unwrap_or_default();
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => port
            .parse()
            .map(|port| (host.to_owned(), port))
            .map_err(|_| format!("{relay} has no valid port.")),
        _ if !rest.is_empty() => Ok((rest.to_owned(), 80)),
        _ => Err(format!("{relay} names no host.")),
    }
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

fn dial(relay: &str, proxy: Option<SocketAddr>) -> Result<WebSocket<TcpStream>, String> {
    let (host, port) = authority(relay)?;
    let stream = match proxy {
        Some(proxy) => through(proxy, &host, port)?,
        None => TcpStream::connect((host.as_str(), port))
            .map_err(|error| format!("{relay} did not accept a connection: {error}."))?,
    };
    // The handshake gets the patience; once it is done a read gives up every tick.
    stream.set_read_timeout(Some(PATIENCE)).ok();
    stream.set_write_timeout(Some(PATIENCE)).ok();
    let (socket, _) = tungstenite::client(relay, stream)
        .map_err(|error| format!("{relay} did not complete a websocket handshake: {error}."))?;
    socket.get_ref().set_read_timeout(Some(TICK)).ok();
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

/// Read the live feed of `relay` for the subscriber key `secret`, with `filter`, and call
/// `on_event` for every event it sends, until it ends. `on_event` returns whether to go on.
pub fn read(
    relay: &str,
    secret: &[u8; 32],
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
    let answer = match event::sign(
        secret,
        event::now(),
        CLIENT_AUTH,
        json!([["relay", relay], ["challenge", challenge]]),
        "",
    ) {
        Ok(answer) => answer,
        Err(error) => return Ended::Dropped(error.message),
    };
    let mut filter = filter.clone();
    if let Some(filter) = filter.as_object_mut() {
        // A filter's `limit` has no meaning for a live feed.
        filter.remove("limit");
    }
    for message in [
        json!(["AUTH", answer]),
        json!(["REQ", SUBSCRIPTION, filter]),
    ] {
        if let Err(error) = socket.send(Message::text(message.to_string())) {
            return dropped(error);
        }
    }

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
            (Some("EVENT"), Some(id)) if id == SUBSCRIPTION => {
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
