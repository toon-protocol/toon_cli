//! A relay that holds the events a test hands it, for the commands that read a Listing and
//! its Provider Profile. It answers a `REQ` with the stored events that match the filter
//! (`kinds`, `authors` and the single-letter tag filters) and `EOSE`, and accepts nothing.

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;

use k256::schnorr::SigningKey;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tungstenite::Message;

#[derive(Clone)]
pub struct FakeDirectory {
    address: SocketAddr,
    events: Arc<Mutex<Vec<Value>>>,
}

/// The public key, in hex, of the key whose secret is 32 bytes of `byte`.
pub fn public_key(byte: u8) -> String {
    let key = SigningKey::from_bytes(&[byte; 32]).expect("a key");
    hex::encode(key.verifying_key().to_bytes())
}

/// An event of `kind` signed by the key whose secret is 32 bytes of `byte`.
pub fn signed(byte: u8, created_at: u64, kind: u64, tags: Value, content: &str) -> Value {
    let key = SigningKey::from_bytes(&[byte; 32]).expect("a key");
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let serialized = json!([0, pubkey, created_at, kind, tags, content]).to_string();
    let id: [u8; 32] = Sha256::digest(serialized.as_bytes()).into();
    let signature = key.sign_raw(&id, &[0u8; 32]).expect("a signature");
    json!({
        "id": hex::encode(id),
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
        "sig": hex::encode(signature.to_bytes()),
    })
}

fn matches(filter: &Value, event: &Value) -> bool {
    let among = |field: &str, value: &Value| {
        filter[field]
            .as_array()
            .is_none_or(|allowed| allowed.contains(value))
    };
    if !among("kinds", &event["kind"]) || !among("authors", &event["pubkey"]) {
        return false;
    }
    filter
        .as_object()
        .into_iter()
        .flatten()
        .all(|(key, wanted)| {
            let Some(name) = key.strip_prefix('#') else {
                return true;
            };
            event["tags"].as_array().is_some_and(|tags| {
                tags.iter().any(|tag| {
                    tag[0] == name && wanted.as_array().is_some_and(|w| w.contains(&tag[1]))
                })
            })
        })
}

impl FakeDirectory {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let directory = Self {
            address: listener.local_addr().expect("address"),
            events: Arc::default(),
        };
        let served = directory.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = served.clone();
                thread::spawn(move || {
                    let Ok(mut socket) = tungstenite::accept(stream) else {
                        return;
                    };
                    while let Ok(message) = socket.read() {
                        let Message::Text(text) = message else {
                            continue;
                        };
                        let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        if frame.first().and_then(Value::as_str) != Some("REQ") {
                            continue;
                        }
                        let (id, filter) = (frame[1].clone(), frame[2].clone());
                        let events = served.events.lock().unwrap().clone();
                        for event in events.iter().filter(|event| matches(&filter, event)) {
                            let _ =
                                socket.send(Message::text(json!(["EVENT", id, event]).to_string()));
                        }
                        let _ = socket.send(Message::text(json!(["EOSE", id]).to_string()));
                    }
                });
            }
        });
        directory
    }

    pub fn url(&self) -> String {
        format!("ws://{}", self.address)
    }

    pub fn publish(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }
}
