//! A stand-in for the relay, for the tests of the app runners and of `toon up`.
//!
//! It takes what the relay's image takes (`TOON_BLS_PORT`, `TOON_DATA_DIR`,
//! `NOSTR_SECRET_KEY`), answers `GET /health`, and answers a `POST` to `/write` or
//! `/write-ephemeral` with 200 after appending `<path> <body in hex>` to `writes.log` in its
//! data directory. It writes the secret key it was handed to `environment` there, and the
//! `TOON_RELAY_*` settings it was handed, one `NAME=value` per line, to `settings`. It
//! exits when its standard input closes, as a supervisor's apps do.

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
    let key = env::var("NOSTR_SECRET_KEY").unwrap_or_default();
    fs::write(
        data.join("environment"),
        format!("NOSTR_SECRET_KEY={key}\n"),
    )
    .expect("write");
    let mut settings: Vec<String> = env::vars()
        .filter(|(name, _)| name.starts_with("TOON_RELAY_"))
        .map(|(name, value)| format!("{name}={value}\n"))
        .collect();
    settings.sort();
    fs::write(data.join("settings"), settings.concat()).expect("write");

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

    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for stream in listener.incoming().flatten() {
        let data = data.clone();
        thread::spawn(move || serve(stream, &data));
    }
}

fn serve(stream: TcpStream, data: &Path) {
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
        ("POST", "/write" | "/write-ephemeral") => {
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
