//! `toon event publish` and `toon event query`: the agent takes part in Nostr.
//!
//! An event is signed with the agent identity (ADR 0004), never with a key that pays.
//! It is published as an operator write to the agent node's own relay through the
//! relay's write route, and read back with a plain NIP-01 `REQ`, which is free.

use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use k256::schnorr::SigningKey;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tungstenite::{stream::MaybeTlsStream, Message};

use crate::derive;
use crate::keystore;
use crate::node;
use crate::operator::{self, Answer};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long a relay gets to answer each message of a query.
const PATIENCE: Duration = Duration::from_secs(30);

/// The subscription id of a query's `REQ`.
const SUBSCRIPTION: &str = "toon";

fn usage(message: impl Into<String>) -> Error {
    Error {
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// The event's id: the SHA-256 of its NIP-01 serialization.
fn event_id(pubkey: &str, created_at: u64, kind: u64, tags: &Value, content: &str) -> [u8; 32] {
    let serialized = json!([0, pubkey, created_at, kind, tags, content]).to_string();
    Sha256::digest(serialized.as_bytes()).into()
}

/// A signed event of `kind`, made `created_at`, by the key `secret`.
fn sign(
    secret: &[u8; 32],
    created_at: u64,
    kind: u64,
    tags: Value,
    content: &str,
) -> Result<Value, Error> {
    let key = SigningKey::from_bytes(secret).map_err(|_| Error {
        code: ErrorCode::KeystoreCorrupt,
        message: "The agent identity is not a valid key.".into(),
    })?;
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let id = event_id(&pubkey, created_at, kind, &tags, content);
    let signature = key
        .sign_raw(&id, &keystore::random::<32>()?)
        .map_err(|_| Error {
            code: ErrorCode::Io,
            message: "The event could not be signed.".into(),
        })?;
    Ok(json!({
        "id": hex::encode(id),
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
        "sig": hex::encode(signature.to_bytes()),
    }))
}

/// Tags are a JSON array of arrays of strings.
fn parse_tags(tags: &str) -> Result<Value, Error> {
    let value: Value = serde_json::from_str(tags)
        .map_err(|error| usage(format!("--tags is not JSON: {error}.")))?;
    let well_formed = value.as_array().is_some_and(|tags| {
        tags.iter().all(|tag| {
            tag.as_array()
                .is_some_and(|tag| !tag.is_empty() && tag.iter().all(Value::is_string))
        })
    });
    if well_formed {
        Ok(value)
    } else {
        Err(usage(
            "--tags must be a JSON array of non-empty arrays of strings, like [[\"t\",\"toon\"]].",
        ))
    }
}

/// `toon event publish`: sign an event with the agent identity and write it to the agent
/// node's own relay through the relay's write route.
pub fn publish(
    home: &Path,
    kind: u64,
    content: &str,
    tags: &str,
    amount: u64,
) -> Result<Report, Error> {
    let tags = parse_tags(tags)?;
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let passphrase = keystore::passphrase()?;
    let mnemonic: bip39::Mnemonic =
        keystore::open(home, &passphrase)?
            .parse()
            .map_err(|_| Error {
                code: ErrorCode::KeystoreCorrupt,
                message: "The keystore does not hold a valid mnemonic.".into(),
            })?;
    let secret =
        derive::agent_identity_secret(&*derive::seed(&mnemonic)).map_err(|source| Error {
            code: ErrorCode::KeystoreCorrupt,
            message: source.0,
        })?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let event = sign(&secret, created_at, kind, tags, content)?;

    let body = home.join(format!(
        "event.{}.json",
        hex::encode(keystore::random::<8>()?)
    ));
    node::write(&body, event.to_string().as_bytes(), 0o600)?;
    let answer = operator::dispatch(home, node::RELAY_WRITE_PREFIX, amount, Some(&body));
    let _ = std::fs::remove_file(&body);

    let id = event["id"].as_str().unwrap_or_default().to_owned();
    Ok(match answer? {
        Answer::Fulfilled { status, body } if (200..300).contains(&status) => Report {
            exit: Exit::Success,
            json: json!({
                "outcome": "published",
                "event": event,
                "response": { "status": status, "body": body },
            }),
            text: format!("Published event {id}."),
        },
        Answer::Fulfilled { status, body } => Report {
            exit: Exit::Failure,
            json: json!({
                "outcome": "refused",
                "event": event,
                "response": { "status": status, "body": body },
            }),
            text: format!("The relay refused event {id} with {status}: {body}"),
        },
        Answer::Rejected { code, message } => Report {
            exit: Exit::Failure,
            json: json!({
                "outcome": "rejected",
                "event": event,
                "reject": { "code": code, "message": message },
            }),
            text: format!("Rejected with {code}: event {id}. {message}"),
        },
        Answer::WrongFulfilment => Report {
            exit: Exit::Failure,
            json: json!({ "outcome": "wrong_fulfilment", "event": event }),
            text: format!("Fulfilled, but not by this connector: event {id}."),
        },
    })
}

/// `toon event query`: the stored events of `relay` that match `filter`.
pub fn query(relay: &str, filter: &str) -> Result<Report, Error> {
    let filter: Value = serde_json::from_str(filter)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| usage("--filter must be one JSON object, like {\"kinds\":[1]}."))?;
    let failed = |message: String| Error {
        code: ErrorCode::QueryFailed,
        message,
    };
    if !relay.starts_with("ws://") {
        return Err(failed(format!(
            "{relay} is not a ws:// URL; this build dials plain websocket relays only."
        )));
    }
    let (mut socket, _) = tungstenite::connect(relay)
        .map_err(|error| failed(format!("{relay} did not accept a connection: {error}.")))?;
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        set_timeouts(stream).map_err(|error| failed(format!("{relay}: {error}.")))?;
    }
    socket
        .send(Message::text(
            json!(["REQ", SUBSCRIPTION, filter]).to_string(),
        ))
        .map_err(|error| failed(format!("{relay} did not take the request: {error}.")))?;

    let mut events = Vec::new();
    loop {
        let message = socket
            .read()
            .map_err(|error| failed(format!("{relay} stopped answering: {error}.")))?;
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match (frame.first().and_then(Value::as_str), frame.get(1)) {
            (Some("EVENT"), Some(id)) if id == SUBSCRIPTION => {
                if let Some(event) = frame.get(2) {
                    events.push(event.clone());
                }
            }
            (Some("EOSE"), Some(id)) if id == SUBSCRIPTION => break,
            (Some("CLOSED"), Some(id)) if id == SUBSCRIPTION => {
                let reason = frame.get(2).and_then(Value::as_str).unwrap_or_default();
                return Err(failed(format!("{relay} closed the subscription: {reason}")));
            }
            _ => {}
        }
    }
    let _ = socket.send(Message::text(json!(["CLOSE", SUBSCRIPTION]).to_string()));
    let _ = socket.close(None);

    let text = if events.is_empty() {
        "No stored event matches.".to_owned()
    } else {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "relay": relay, "filter": filter, "events": events }),
        text,
    })
}

fn set_timeouts(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::schnorr::{Signature, VerifyingKey};

    #[test]
    fn an_event_carries_the_id_and_signature_nip_01_defines() {
        let secret = [7u8; 32];
        let event = sign(&secret, 1_700_000_000, 1, json!([["t", "toon"]]), "hi").unwrap();

        let pubkey = event["pubkey"].as_str().unwrap();
        let id = event_id(pubkey, 1_700_000_000, 1, &event["tags"], "hi");
        assert_eq!(event["id"], hex::encode(id));
        let key = VerifyingKey::from_bytes(&hex::decode(pubkey).unwrap()).unwrap();
        let signature = Signature::try_from(
            hex::decode(event["sig"].as_str().unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert!(key.verify_raw(&id, &signature).is_ok());
    }

    #[test]
    fn tags_must_be_arrays_of_strings() {
        assert!(parse_tags("[]").is_ok());
        assert!(parse_tags(r#"[["e","abc","wss://r"]]"#).is_ok());
        assert!(parse_tags(r#"["e"]"#).is_err());
        assert!(parse_tags(r#"[[]]"#).is_err());
        assert!(parse_tags(r#"[[1]]"#).is_err());
        assert!(parse_tags("nope").is_err());
    }
}
