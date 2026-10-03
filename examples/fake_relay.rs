//! A stand-in for the relay, for the tests of the app runners and of `toon up`.
//!
//! It takes what the relay's image takes (`TOON_BLS_PORT`, `TOON_DATA_DIR`,
//! `NOSTR_SECRET_KEY`, and `TOON_RELAY_PORT` for the read port), answers `GET /health`, and
//! appends `<path> <body in hex>` to `writes.log` in its data directory for every `POST`
//! to `/`, `/write` or `/write-ephemeral`. As the relay does, it answers a write with 200
//! only if the body is `{"event": ...}`, and with 400 otherwise; a `POST` to `/`, where
//! it stands in for any other app, is always answered with 200. It writes the secret key
//! it was handed to `environment` there, `TOON_ENFORCE_EXPIRATION`, `TOON_BLOCKED_EVENT_IDS` and the `TOON_RELAY_*`
//! settings it was handed but the read port, one `NAME=value` per line, to `settings`, and
//! `TOON_CONNECTOR_URL`, `TOON_WRITE_ILP_ADDRESS`, `TOON_SUBSCRIBE_ILP_ADDRESS`,
//! `TOON_BROADCAST_PRICE` and `TOON_RELAY_URL` to `connector`. Every `TOON_` name it was
//! handed, one per line, goes to `names`. It answers `GET /subscribers` with
//! `subscribers.json` of its data directory, if there is one. It exits when its standard
//! input closes, as a supervisor's apps do.
//!
//! The event of a write, or a JSON body posted to `/`, is also stored in `events.log`,
//! one per line, and a websocket client on the same port reads them back with a NIP-01
//! `REQ` (`ids`, `authors`, `kinds`, `#<letter>` tags and `limit` are honoured) and gets
//! `EOSE` after the stored events. An addressable event replaces the earlier one at its
//! address.
//!
//! The websocket also keeps a `REQ` open after `EOSE` and sends the events written
//! afterwards. A relay told a broadcast price (`TOON_BROADCAST_PRICE`) sells its feed: it
//! greets a connection with an `AUTH` challenge, and keeps the `REQ` open only for the
//! connection that answers it with its own identity key (`NOSTR_SECRET_KEY`) and a `relay`
//! tag naming the host of `TOON_RELAY_URL`, as the Rust relay does. For anyone else it
//! sends `CLOSED` with `payment-required:` after `EOSE`.
//!
//! A relay told `TOON_NIP17_RECIPIENT_ONLY=true` follows the Rust relay's rule for gift
//! wraps: it challenges every connection and records every key the connection proves; a
//! wrap (kind 1059) is served, stored or live, only to a connection that has proven a key in
//! the wrap's `p` tags; a `REQ` naming kind 1059 from a connection that has proven no key is
//! `CLOSED` with `auth-required:`, and one naming no kinds is answered without wraps. On a
//! relay that sells its feed, proving a key closes the subscriptions the connection had, and
//! the connection is fed live only if the key proven last is the relay's own.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::Duration;

fn main() {
    let port = env::var("TOON_BLS_PORT").expect("TOON_BLS_PORT");
    let data = PathBuf::from(env::var("TOON_DATA_DIR").expect("TOON_DATA_DIR"));
    // `apps/fail-<name>` beside the app's directory makes that app not start, for a test of
    // what a failed start leaves.
    if let Some(apps) = data.parent().and_then(Path::parent) {
        let name = data.parent().and_then(Path::file_name).unwrap_or_default();
        let mut marker = std::ffi::OsString::from("fail-");
        marker.push(name);
        if apps.join(marker).exists() {
            process::exit(1);
        }
    }
    let key = env::var("NOSTR_SECRET_KEY").unwrap_or_default();
    fs::write(
        data.join("environment"),
        format!("NOSTR_SECRET_KEY={key}\n"),
    )
    .expect("write");
    let mut settings: Vec<String> = env::vars()
        .filter(|(name, _)| {
            (name.starts_with("TOON_RELAY_") && name != "TOON_RELAY_PORT")
                || name == "TOON_BLOCKED_EVENT_IDS"
                || name == "TOON_ENFORCE_EXPIRATION"
        })
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect();
    settings.sort();
    fs::write(data.join("settings"), settings.concat()).expect("write");
    let mut names: Vec<String> = env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("TOON_"))
        .map(|name| format!("{name}\n"))
        .collect();
    names.sort();
    fs::write(data.join("names"), names.concat()).expect("write");
    // What it was told of its connector, to `connector`, in the same form.
    let connector: String = [
        "TOON_CONNECTOR_URL",
        "TOON_WRITE_ILP_ADDRESS",
        "TOON_SUBSCRIBE_ILP_ADDRESS",
        "TOON_BROADCAST_PRICE",
        "TOON_RELAY_URL",
    ]
    .iter()
    .filter_map(|name| env::var(name).ok().map(|value| format!("{name}={value}\n")))
    .collect();
    fs::write(data.join("connector"), connector).expect("write");

    thread::spawn(|| {
        let mut ignored = [0u8; 64];
        let mut stdin = std::io::stdin().lock();
        while matches!(stdin.read(&mut ignored), Ok(read) if read > 0) {}
        process::exit(0);
    });
    if env::var_os("FAKE_RELAY_EXIT_AFTER_START").is_some() {
        thread::spawn(|| {
            thread::sleep(Duration::from_millis(300));
            process::exit(1);
        });
    }

    // The read port answers as the write port does, so a test can reach it through the
    // overlay.
    if let Ok(read) = env::var("TOON_RELAY_PORT") {
        let reads = TcpListener::bind(format!("127.0.0.1:{read}")).expect("bind the read port");
        let data = data.clone();
        thread::spawn(move || {
            for stream in reads.incoming().flatten() {
                let data = data.clone();
                thread::spawn(move || serve(stream, &data));
            }
        });
    }
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for stream in listener.incoming().flatten() {
        let data = data.clone();
        thread::spawn(move || serve(stream, &data));
    }
}

fn serve(stream: TcpStream, data: &Path) {
    let mut start = [0u8; 1024];
    let peeked = stream.peek(&mut start).unwrap_or(0);
    if String::from_utf8_lossy(&start[..peeked])
        .to_ascii_lowercase()
        .contains("upgrade: websocket")
    {
        return websocket(stream, data);
    }
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut parts = request.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    );
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);

    let posted = method == "POST" && matches!(path.as_str(), "/" | "/write" | "/write-ephemeral");
    if posted {
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("writes.log"))
            .expect("open the write log");
        let _ = writeln!(
            log,
            "{path} {}",
            body.iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
    // A write carries its event in the body's `event` field. A body without one is
    // refused in the words the relay's image uses. Anything posted to `/`, where this
    // stands in for any other app, is taken.
    let to_relay = posted && path != "/";
    let json = serde_json::from_slice::<serde_json::Value>(&body);
    let subscribers = fs::read_to_string(data.join("subscribers.json"));
    let (status, answer) = if method == "GET" && path == "/health" {
        ("200 OK", "ok")
    } else if method == "GET" && path == "/subscribers" && subscribers.is_ok() {
        ("200 OK", subscribers.as_deref().unwrap_or_default())
    } else if !posted {
        ("404 Not Found", "not found")
    } else if to_relay && json.is_err() {
        ("400 Bad Request", r#"{"error":"Invalid request body"}"#)
    } else if to_relay && json.as_ref().is_ok_and(|json| json["event"].is_null()) {
        (
            "400 Bad Request",
            r#"{"error":"Missing required field: event"}"#,
        )
    } else {
        match json {
            Ok(mut json) if to_relay => store(data, json["event"].take()),
            Ok(event) => store(data, event),
            Err(_) => {}
        }
        ("200 OK", "stored")
    };
    let _ = write!(
        reader.get_mut(),
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
}

/// The value of the first `d` tag of `event`: what an addressable event is addressed by.
fn identifier(event: &serde_json::Value) -> Option<&str> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == "d")
        .and_then(|tag| tag[1].as_str())
}

/// Append `event` to the event log. As NIP-01 says of an addressable kind (30000 to
/// 39999), only the newest event of an address is kept, and of two made in the same
/// second the one with the lower id.
fn store(data: &Path, event: serde_json::Value) {
    let path = data.join("events.log");
    let mut kept: Vec<serde_json::Value> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if event["kind"]
        .as_u64()
        .is_some_and(|kind| (30000..40000).contains(&kind))
    {
        let same_address = |other: &serde_json::Value| {
            other["kind"] == event["kind"]
                && other["pubkey"] == event["pubkey"]
                && identifier(other) == identifier(&event)
        };
        let newer = |a: &serde_json::Value, b: &serde_json::Value| {
            (a["created_at"].as_u64(), b["id"].as_str())
                > (b["created_at"].as_u64(), a["id"].as_str())
        };
        if kept
            .iter()
            .any(|other| same_address(other) && !newer(&event, other))
        {
            return;
        }
        kept.retain(|other| !same_address(other));
    }
    kept.push(event);
    let lines: String = kept.iter().map(|event| format!("{event}\n")).collect();
    fs::write(path, lines).expect("write the event log");
}

/// Whether `event` is among what `filter` asks for.
fn matches(filter: &serde_json::Value, event: &serde_json::Value) -> bool {
    let fields = [("ids", "id"), ("authors", "pubkey"), ("kinds", "kind")]
        .iter()
        .all(|(wanted, field)| match filter[wanted].as_array() {
            Some(allowed) => allowed.contains(&event[field]),
            None => true,
        });
    // A `#x` entry asks for an event with a tag `x` that has one of the values.
    let tags = filter.as_object().is_none_or(|filter| {
        filter.iter().all(|(name, wanted)| {
            let (Some(letter), Some(wanted)) = (name.strip_prefix('#'), wanted.as_array()) else {
                return true;
            };
            event["tags"].as_array().is_some_and(|tags| {
                tags.iter()
                    .any(|tag| tag[0] == letter && wanted.contains(&tag[1]))
            })
        })
    });
    fields && tags
}

/// What a client's websocket is told, and by whom it is allowed a live read.
struct Gate {
    /// The challenge sent to the connection, if the relay sells its feed.
    challenge: Option<String>,
    /// Whether the connection answered it as the relay's operator.
    operator: bool,
    /// Whether a wrap is served only to the keys it is addressed to, which are those the
    /// connection has proven.
    recipient_only: bool,
    /// Every key the connection has proven.
    proven: Vec<String>,
}

/// Whether `gate` lets its connection read `event`.
fn readable(gate: &Gate, event: &serde_json::Value) -> bool {
    !gate.recipient_only
        || event["kind"] != 1059
        || event["tags"].as_array().is_some_and(|tags| {
            tags.iter()
                .any(|tag| tag[0] == "p" && gate.proven.iter().any(|key| tag[1] == key.as_str()))
        })
}

/// The host of a URL: what stands between `://` and the next `/` or `:`.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', ':']).next().unwrap_or_default()
}

/// The reason an `AUTH` event is not a key answering `challenge`, naming the host the relay
/// is reached at, as the relay checks it (`docs/paid-feed.md`, "The operator"), or none if
/// it is.
fn refused(event: &serde_json::Value, challenge: &str) -> Option<&'static str> {
    use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
    use sha2::{Digest, Sha256};

    let tag = |name: &str| {
        event["tags"]
            .as_array()
            .and_then(|tags| tags.iter().find(|tag| tag[0] == name))
            .and_then(|tag| tag[1].as_str())
    };
    let serialized = serde_json::json!([
        0,
        event["pubkey"],
        event["created_at"],
        event["kind"],
        event["tags"],
        event["content"]
    ])
    .to_string();
    let id = Sha256::digest(serialized.as_bytes());
    let signed = hex::decode(event["sig"].as_str().unwrap_or_default())
        .ok()
        .and_then(|sig| Signature::try_from(sig.as_slice()).ok())
        .zip(
            hex::decode(event["pubkey"].as_str().unwrap_or_default())
                .ok()
                .and_then(|key| VerifyingKey::from_bytes(&key).ok()),
        )
        .is_some_and(|(sig, key)| key.verify_prehash(&id, &sig).is_ok());
    let reached = env::var("TOON_RELAY_URL").unwrap_or_default();
    if event["kind"] != 22242 || hex::encode(id) != event["id"].as_str().unwrap_or_default() {
        Some("invalid: not an AUTH event")
    } else if !signed {
        Some("invalid: bad signature")
    } else if tag("challenge") != Some(challenge) {
        Some("invalid: wrong challenge")
    } else if env::var_os("TOON_BROADCAST_PRICE").is_none() {
        // A relay that sells nothing knows no URL of its own: the tag need only name one.
        tag("relay")
            .filter(|url| !url.is_empty())
            .map_or(Some("invalid: the event names no relay"), |_| None)
    } else if tag("relay").map(host) != Some(host(&reached)) {
        Some("invalid: the relay tag does not name the host this relay is reached at")
    } else {
        None
    }
}

/// The relay's own identity key, in hex.
fn own_key() -> String {
    let secret = hex::decode(env::var("NOSTR_SECRET_KEY").unwrap_or_default()).unwrap_or_default();
    k256::schnorr::SigningKey::from_bytes(&secret)
        .map(|key| hex::encode(key.verifying_key().to_bytes()))
        .unwrap_or_default()
}

fn websocket(stream: TcpStream, data: &Path) {
    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    // A relay that sells its feed (it was told a broadcast price) greets with a challenge
    // and keeps a `REQ` open after `EOSE` only for its operator. Any other relay keeps
    // it open for every reader.
    let selling = env::var_os("TOON_BROADCAST_PRICE").is_some();
    let recipient_only = env::var("TOON_NIP17_RECIPIENT_ONLY").is_ok_and(|value| value == "true");
    let mut gate = Gate {
        challenge: (selling || recipient_only).then(|| hex::encode(rand_bytes())),
        operator: !selling,
        recipient_only,
        proven: Vec::new(),
    };
    if let Some(challenge) = &gate.challenge {
        let _ = socket.send(tungstenite::Message::text(
            serde_json::json!(["AUTH", challenge]).to_string(),
        ));
    }
    // Reads give up every tick, so that an open `REQ` notices the events written meanwhile.
    let _ = socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)));
    // The open `REQ`s: id, filters, and the events already sent for it.
    let mut open: Vec<(
        serde_json::Value,
        Vec<serde_json::Value>,
        Vec<serde_json::Value>,
    )> = Vec::new();
    loop {
        for (subscription, filters, sent) in &mut open {
            for event in stored(data) {
                if !sent.contains(&event["id"])
                    && readable(&gate, &event)
                    && filters.iter().any(|f| matches(f, &event))
                {
                    sent.push(event["id"].clone());
                    let _ = socket.send(tungstenite::Message::text(
                        serde_json::json!(["EVENT", subscription, event]).to_string(),
                    ));
                }
            }
        }
        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return,
        };
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        let Ok(serde_json::Value::Array(frame)) = serde_json::from_str(&text) else {
            continue;
        };
        match frame.first().and_then(|kind| kind.as_str()) {
            Some("AUTH") if frame.len() == 2 => {
                let reason = match &gate.challenge {
                    Some(challenge) => refused(&frame[1], challenge),
                    None => Some("invalid: no challenge was sent"),
                };
                if reason.is_none() {
                    let key = frame[1]["pubkey"].as_str().unwrap_or_default().to_owned();
                    // A connection that proves another key loses what it had open.
                    if selling && gate.proven.last() != Some(&key) {
                        open.clear();
                    }
                    // A relay that sells nothing feeds every reader live.
                    gate.operator = !selling || key == own_key();
                    gate.proven.push(key);
                }
                let _ = socket.send(tungstenite::Message::text(
                    serde_json::json!([
                        "OK",
                        frame[1]["id"],
                        reason.is_none(),
                        reason.unwrap_or("")
                    ])
                    .to_string(),
                ));
            }
            Some("CLOSE") if frame.len() == 2 => open.retain(|(id, _, _)| *id != frame[1]),
            Some("REQ") if frame.len() >= 3 => {
                let subscription = frame[1].clone();
                open.retain(|(id, _, _)| *id != subscription);
                let names_wraps = frame[2..].iter().any(|filter| {
                    filter["kinds"]
                        .as_array()
                        .is_some_and(|k| k.contains(&1059.into()))
                });
                if gate.recipient_only && names_wraps && gate.proven.is_empty() {
                    let _ = socket.send(tungstenite::Message::text(
                        serde_json::json!([
                            "CLOSED",
                            subscription,
                            "auth-required: a gift wrap is read by the key it is addressed to"
                        ])
                        .to_string(),
                    ));
                    continue;
                }
                let events = stored(data);
                let mut found: Vec<&serde_json::Value> = Vec::new();
                for filter in &frame[2..] {
                    // A filter's `limit` keeps its newest matches: the log holds them oldest first.
                    let matched: Vec<_> = events
                        .iter()
                        .filter(|event| readable(&gate, event) && matches(filter, event))
                        .collect();
                    let limit = filter["limit"]
                        .as_u64()
                        .map_or(matched.len(), |limit| limit as usize);
                    for event in &matched[matched.len().saturating_sub(limit)..] {
                        if !found.contains(event) {
                            found.push(event);
                        }
                    }
                }
                let mut sent = Vec::new();
                for event in found {
                    sent.push(event["id"].clone());
                    let _ = socket.send(tungstenite::Message::text(
                        serde_json::json!(["EVENT", subscription, event]).to_string(),
                    ));
                }
                let _ = socket.send(tungstenite::Message::text(
                    serde_json::json!(["EOSE", subscription]).to_string(),
                ));
                if gate.operator {
                    // Everything already stored counts as sent; later writes follow.
                    sent.extend(events.iter().map(|event| event["id"].clone()));
                    open.push((subscription, frame[2..].to_vec(), sent));
                } else {
                    let _ = socket.send(tungstenite::Message::text(
                        serde_json::json!([
                            "CLOSED",
                            subscription,
                            "payment-required: this feed is for a subscriber with a balance"
                        ])
                        .to_string(),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// The events of the event log, oldest first.
fn stored(data: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(data.join("events.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn rand_bytes() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let _ = fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    bytes
}
