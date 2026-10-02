//! `toon event publish` and `toon event query`: the agent takes part in Nostr.
//!
//! An event is signed with the agent identity (ADR 0004), never with a key that pays.
//! It is published as an operator write to the agent node's own relay through the
//! relay's write route, and read back with a plain NIP-01 `REQ`, which is free.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use k256::schnorr::SigningKey;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tungstenite::Message;

use crate::cli::EventCommand;
use crate::derive;
use crate::egress::Egress;
use crate::feed;
use crate::home;
use crate::keystore;
use crate::node;
use crate::operator::{self, Answer};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::relay_url::{self, RelayUrl};
use crate::spending;
use crate::tls;

/// How long a relay gets to answer each message of a query.
const PATIENCE: Duration = Duration::from_secs(30);

/// The subscription id of a query's `REQ`.
const SUBSCRIPTION: &str = "toon";

fn usage(message: impl Into<String>) -> Error {
    Error {
        nothing_sent: false,
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
pub fn sign(
    secret: &[u8; 32],
    created_at: u64,
    kind: u64,
    tags: Value,
    content: &str,
) -> Result<Value, Error> {
    let key = SigningKey::from_bytes(secret).map_err(|_| Error {
        nothing_sent: false,
        code: ErrorCode::KeystoreCorrupt,
        message: "The agent identity is not a valid key.".into(),
    })?;
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let id = event_id(&pubkey, created_at, kind, &tags, content);
    let signature = key
        .sign_raw(&id, &keystore::random::<32>()?)
        .map_err(|_| Error {
            nothing_sent: false,
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
    let secret = agent_secret(home)?;
    let event = sign(&secret, now(), kind, tags, content)?;
    write(home, event, amount)
}

/// The present time, in seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The wallet's seed, opened with the passphrase.
pub fn wallet_seed(home: &Path) -> Result<zeroize::Zeroizing<[u8; 64]>, Error> {
    let passphrase = keystore::passphrase()?;
    let mnemonic: bip39::Mnemonic =
        keystore::open(home, &passphrase)?
            .parse()
            .map_err(|_| Error {
                nothing_sent: false,
                code: ErrorCode::KeystoreCorrupt,
                message: "The keystore does not hold a valid mnemonic.".into(),
            })?;
    Ok(derive::seed(&mnemonic))
}

/// The agent identity's secret, opened with the passphrase (ADR 0004).
pub fn agent_secret(home: &Path) -> Result<zeroize::Zeroizing<[u8; 32]>, Error> {
    derive::agent_identity_secret(&*wallet_seed(home)?).map_err(|source| Error {
        nothing_sent: false,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })
}

/// The x-only public key, in hex, of the key `secret`.
pub fn public_key(secret: &[u8; 32]) -> Result<String, Error> {
    SigningKey::from_bytes(secret)
        .map(|key| hex::encode(key.verifying_key().to_bytes()))
        .map_err(|_| Error {
            nothing_sent: false,
            code: ErrorCode::KeystoreCorrupt,
            message: "The agent identity is not a valid key.".into(),
        })
}

/// Write a signed event to the agent node's own relay through the relay's write route.
pub fn write(home: &Path, event: Value, amount: u64) -> Result<Report, Error> {
    // The relay of the TOON app that fronts the agent node's connector. With none, the
    // write is addressed as it would be and the connector rejects it.
    let state = node::State::load(home)?.ok_or_else(|| node::no_agent_node(home))?;
    write_to(
        home,
        event,
        &state.toon_apps[0].relay_prefix(),
        amount,
        None,
    )
}

/// Write a signed event to `destination` for `amount`, sealed to the key `seal_to`, or to
/// this agent node's own connector.
fn write_to(
    home: &Path,
    event: Value,
    destination: &str,
    amount: u64,
    seal_to: Option<&[u8; 65]>,
) -> Result<Report, Error> {
    let answer = match seal_to {
        Some(key) => {
            let headers = vec![("content-type".to_owned(), "application/json".to_owned())];
            operator::dispatch_with_headers(
                home,
                destination,
                amount,
                key,
                headers,
                event.to_string().into_bytes(),
            )
        }
        None => {
            let body = home.join(format!(
                "event.{}.json",
                hex::encode(keystore::random::<8>()?)
            ));
            node::write(&body, event.to_string().as_bytes(), 0o600)?;
            let answer = operator::dispatch(home, destination, amount, None, Some(&body));
            let _ = std::fs::remove_file(&body);
            answer
        }
    };

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
    let events = fetch(&Egress::open()?, relay, &filter)?;
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

/// The stored events of `relay` that match `filter`, read with a NIP-01 `REQ`.
pub fn fetch(egress: &Egress, relay: &str, filter: &Value) -> Result<Vec<Value>, Error> {
    let failed = |message: String| Error {
        nothing_sent: false,
        code: ErrorCode::QueryFailed,
        message,
    };
    RelayUrl::parse(relay).map_err(failed)?;
    let proxy = egress.proxy_for(relay)?;
    let mut socket = feed::dial(relay, proxy).map_err(failed)?;
    feed::set_timeouts(&socket, PATIENCE, PATIENCE)
        .map_err(|error| failed(format!("{relay}: {error}.")))?;
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

    Ok(events)
}

/// Where another relay is paid for a write, from its information document.
pub struct Edge {
    pub ilp_address: String,
    /// A location hint: where to peer, never dialled by a write.
    pub connector_url: String,
    /// The connector's sealing public key, which a write is sealed to.
    pub seal_key: [u8; 65],
    pub price: u64,
}

/// The write edge in a `toon` object, or what the object is missing to be one: the
/// key is 65 bytes of hex, uncompressed so it begins `04`, with or without `0x`.
pub fn edge_fields(toon: &Value) -> Result<Edge, String> {
    let text = |field: &str| {
        toon[field]
            .as_str()
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("no `{field}`"))
    };
    let key = text("connector_seal_key")?;
    let seal_key: [u8; 65] = hex::decode(key.strip_prefix("0x").unwrap_or(key))
        .ok()
        .and_then(|key| key.try_into().ok())
        .filter(|key: &[u8; 65]| key[0] == 0x04)
        .ok_or_else(|| {
            format!("a `connector_seal_key` that is not 65 bytes of hex beginning `04`: {key}")
        })?;
    Ok(Edge {
        ilp_address: text("ilp_address")?.to_owned(),
        connector_url: text("connector_url")?.to_owned(),
        seal_key,
        price: toon["price"]
            .as_u64()
            .ok_or_else(|| "no `price`".to_owned())?,
    })
}

pub fn unpayable(message: String) -> Error {
    Error {
        nothing_sent: false,
        code: ErrorCode::RelayNotPayable,
        message,
    }
}

/// The HTTP form of a relay URL: where its information document is served, `http://` for
/// a `ws://` relay and `https://` for a `wss://` one.
pub fn http_url(relay: &str) -> Result<String, Error> {
    relay_url::http_form(relay).map_err(unpayable)
}

/// The NIP-11 information document of `relay`, served at its own URL over `http://` or,
/// for a `wss://` relay, `https://`.
pub fn information_document(egress: &Egress, relay: &str) -> Result<Value, Error> {
    let url = http_url(relay)?;
    egress
        .relay_client(&url, PATIENCE)?
        .get(&url)
        .header("accept", "application/nostr+json")
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| {
            let certificate = tls::is_certificate(&error);
            let error = tls::chain(&error);
            unpayable(if certificate {
                format!("The certificate of {relay} did not verify: {error}.")
            } else {
                format!("The information document of {relay} could not be read: {error}.")
            })
        })
}

/// The write edge `relay` publishes in its NIP-11 information document, the `toon`
/// object.
fn edge(egress: &Egress, relay: &str) -> Result<Edge, Error> {
    let document = information_document(egress, relay)?;
    edge_fields(&document["toon"]).map_err(|missing| {
        unpayable(format!(
            "The information document of {relay} does not say where a write is paid for: \
             its `toon` object needs an `ilp_address`, a `connector_url`, a \
             `connector_seal_key` of 65 bytes of hex and a `price`, and has {missing}."
        ))
    })
}

/// `toon event publish --relay`: publish to a relay this agent node does not run, paying
/// its price through this agent node's own connector over a peering. It never creates the
/// peering.
fn publish_to(
    home: &Path,
    relay: &str,
    kind: u64,
    content: &str,
    tags: &str,
    yes: bool,
) -> Result<Report, Error> {
    let tags = parse_tags(tags)?;
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let edge = edge(&Egress::of(home)?, relay)?;
    if !operator::forwards(home, &edge.ilp_address)? {
        return Err(Error {
            nothing_sent: false,
            code: ErrorCode::PeeringNeeded,
            message: format!(
                "No peering of this agent node reaches {}, where {relay} is paid. A peering is \
                 needed: run `toon peer add {}` and then `toon route add {} --peer <id>`.",
                edge.ilp_address, edge.connector_url, edge.ilp_address
            ),
        });
    }
    if !yes {
        return Err(Error {
            nothing_sent: false,
            code: ErrorCode::NotConfirmed,
            message: format!(
                "A write to {relay} costs {} base units. Add `--yes` to say that you mean it.",
                edge.price
            ),
        });
    }
    let secret = agent_secret(home)?;
    let event = sign(&secret, now(), kind, tags, content)?;
    spending::spend(home, edge.price.into(), yes, || {
        let mut report = write_to(
            home,
            event,
            &edge.ilp_address,
            edge.price,
            Some(&edge.seal_key),
        )?;
        // A fulfilled packet moved money, whatever the relay or its fulfilment said.
        let paid = report.json["outcome"] != "rejected";
        report.json["relay"] = json!(relay);
        report.json["paid"] = json!(if paid { edge.price } else { 0 });
        if paid {
            report.text = format!("{} Paid {} base units to {relay}.", report.text, edge.price);
        }
        Ok((report, paid))
    })
}

/// `toon event`.
pub fn run(command: EventCommand) -> Result<Report, Error> {
    match command {
        EventCommand::Publish {
            kind,
            content,
            tags,
            amount: _,
            relay: Some(relay),
            yes,
        } => publish_to(&home::resolve()?, &relay, kind, &content, &tags, yes),
        EventCommand::Publish {
            kind,
            content,
            tags,
            amount,
            relay: None,
            yes: _,
        } => publish(&home::resolve()?, kind, &content, &tags, amount),
        EventCommand::Query { relay, filter } => query(&relay, &filter),
        EventCommand::Follow { relay } => crate::subscribe::follow(&home::resolve()?, &relay),
    }
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
