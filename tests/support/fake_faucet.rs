//! A faucet that answers the way the devnet's does, and funds a `FakeChain` when asked.
//!
//! It serves `POST /api/base-sepolia/request` and `POST /api/solana/usdc-request`,
//! each with `{"address": ...}`, and keeps what it was asked.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// What a faucet answers for a path it refuses, as the devnet's does inside a cooldown.
const REFUSAL: &str = r#"{"message":"address is in its cooldown"}"#;

pub struct FakeFaucet {
    url: String,
    asked: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakeFaucet {
    /// A faucet whose drips fund the chain behind `funded`.
    pub fn funding(funded: Arc<AtomicBool>) -> Self {
        Self::refusing(funded, &[])
    }

    /// A faucet that refuses, with 429, every request to one of `refused` paths, and
    /// funds the chain behind `funded` for the others.
    pub fn refusing(funded: Arc<AtomicBool>, refused: &[&str]) -> Self {
        let refused: Vec<String> = refused.iter().map(|path| (*path).to_owned()).collect();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake faucet");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&asked);
        thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                serve(stream, &funded, &record, &refused);
            }
        });
        Self { url, asked }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The `(path, address)` of every request so far.
    pub fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

fn serve(
    mut stream: TcpStream,
    funded: &AtomicBool,
    asked: &Mutex<Vec<(String, String)>>,
    refused: &[String],
) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    let (head_end, length) = loop {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&request).into_owned();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while request.len() < head_end + length {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let path = text
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let body: serde_json::Value = serde_json::from_slice(&request[head_end..]).unwrap_or_default();
    let address = body["address"].as_str().unwrap_or_default().to_owned();
    let refuse = refused.contains(&path);
    asked.lock().unwrap().push((path, address));
    let (status, reply) = if refuse {
        ("429 Too Many Requests", REFUSAL)
    } else {
        funded.store(true, Ordering::SeqCst);
        ("200 OK", r#"{"success":true}"#)
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
}
