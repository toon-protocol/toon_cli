//! A stand-in for the relay, for the tests of the app runners and of `toon up`.
//!
//! It takes what the relay's image takes (`TOON_BLS_PORT`, `TOON_DATA_DIR`,
//! `NOSTR_SECRET_KEY`, and `TOON_RELAY_PORT` for the read port), answers `GET /health`, and
//! answers a `POST` to `/`, `/write` or
//! `/write-ephemeral` with 200 after appending `<path> <body in hex>` to `writes.log` in its
//! data directory. It writes the secret key it was handed to `environment` there, the
//! `TOON_RELAY_*` settings it was handed but the read port, one `NAME=value` per line, to
//! `settings`, and `TOON_CONNECTOR_URL` and `TOON_WRITE_ILP_ADDRESS` to `connector`. It
//! exits when its standard input closes, as a supervisor's apps do.
//!
//! A body that is a JSON event is also stored in `events.log`, one per line, and a websocket
//! client on the same port reads them back with a NIP-01 `REQ` (`ids`, `authors`, `kinds`,
//! `#<letter>` tags and `limit` are honoured) and gets `EOSE` after the stored events. An
//! addressable event replaces the earlier one at its address.

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
        .filter(|(name, _)| name.starts_with("TOON_RELAY_") && name != "TOON_RELAY_PORT")
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect();
    settings.sort();
    fs::write(data.join("settings"), settings.concat()).expect("write");
    // What it was told of its connector, to `connector`, in the same form.
    let connector: String = ["TOON_CONNECTOR_URL", "TOON_WRITE_ILP_ADDRESS"]
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

    let (status, answer) = match (method.as_str(), path.as_str()) {
        ("GET", "/health") => ("200 OK", "ok"),
        ("POST", "/" | "/write" | "/write-ephemeral") => {
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
            if let Ok(event) = serde_json::from_slice::<serde_json::Value>(&body) {
                store(data, event);
            }
            ("200 OK", "stored")
        }
        _ => ("404 Not Found", "not found"),
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

fn websocket(stream: TcpStream, data: &Path) {
    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    while let Ok(message) = socket.read() {
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        let Ok(serde_json::Value::Array(frame)) = serde_json::from_str(&text) else {
            continue;
        };
        if frame.first().and_then(|kind| kind.as_str()) != Some("REQ") || frame.len() < 3 {
            continue;
        }
        let subscription = frame[1].clone();
        let stored = fs::read_to_string(data.join("events.log")).unwrap_or_default();
        let events: Vec<serde_json::Value> = stored
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let mut found: Vec<&serde_json::Value> = Vec::new();
        for filter in &frame[2..] {
            // A filter's `limit` keeps its newest matches: the log holds them oldest first.
            let matched: Vec<_> = events
                .iter()
                .filter(|event| matches(filter, event))
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
        for event in found {
            let _ = socket.send(tungstenite::Message::text(
                serde_json::json!(["EVENT", subscription, event]).to_string(),
            ));
        }
        let _ = socket.send(tungstenite::Message::text(
            serde_json::json!(["EOSE", subscription]).to_string(),
        ));
    }
}
