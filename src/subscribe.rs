//! `toon relay subscribe` and `toon relay subscriptions`: the operator buys another
//! relay's live feed (ADR 0005, `nips/paid-subscription.md`).
//!
//! Every payment is a packet to the relay's subscribe route, authorized by NIP-98 with the
//! subscriber key, a key the wallet derives for this and not the agent identity. The route
//! credits its price once per packet, so an amount is paid as whole packets. What the
//! relay answered is kept in `subscriptions.json`; `subscriptions` asks each relay for its
//! balance again and shows what was last kept for one that does not answer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::derive;
use crate::event;
use crate::node;
use crate::operator::{self, Answer};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::spending;

/// How long a relay gets to answer a read of a balance.
const PATIENCE: Duration = Duration::from_secs(30);

/// NIP-98: a kind of event that authorizes one HTTP request.
const HTTP_AUTH: u64 = 27235;

fn usage(message: impl Into<String>) -> Error {
    Error {
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// What a relay says it sells its feed for, from its information document.
struct Terms {
    /// Where the relay's connector is reached, for sealing a packet to it.
    connector_url: String,
    /// The subscribe route.
    address: String,
    /// What one packet costs and credits.
    price: u64,
    /// What the relay debits for each event it broadcasts.
    broadcast_price: u64,
}

fn terms(relay: &str) -> Result<Terms, Error> {
    let document = event::information_document(relay)?;
    let subscription = &document["toon_subscription"];
    fn text(value: &Value) -> Option<&str> {
        value.as_str().filter(|text| !text.is_empty())
    }
    let positive = |value: &Value| value.as_u64().filter(|number| *number > 0);
    match (
        text(&document["toon"]["connector_url"]),
        text(&subscription["ilp_address"]),
        positive(&subscription["price"]),
        positive(&subscription["broadcast_price"]),
    ) {
        (Some(connector_url), Some(address), Some(price), Some(broadcast_price)) => Ok(Terms {
            connector_url: connector_url.to_owned(),
            address: address.to_owned(),
            price,
            broadcast_price,
        }),
        _ => Err(event::unpayable(format!(
            "The information document of {relay} has no `toon_subscription` with an \
             `ilp_address`, a `price` and a `broadcast_price` above 0 beside a `toon` with a \
             `connector_url`, so it does not sell its feed."
        ))),
    }
}

/// The subscriber key's secret, opened with the passphrase.
fn subscriber_secret(home: &Path) -> Result<zeroize::Zeroizing<[u8; 32]>, Error> {
    derive::subscriber_secret(&*event::wallet_seed(home)?).map_err(|source| Error {
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })
}

/// The `Authorization` header of a request: NIP-98 authorization signed with `secret`. A
/// request inside a packet has no URL of its own, so `url` names the relay it is meant for.
fn authorization(
    secret: &[u8; 32],
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> Result<String, Error> {
    let mut tags = vec![json!(["u", url]), json!(["method", method])];
    if let Some(body) = body {
        tags.push(json!(["payload", hex::encode(Sha256::digest(body))]));
    }
    let signed = event::sign(secret, event::now(), HTTP_AUTH, Value::Array(tags), "")?;
    Ok(format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(signed.to_string())
    ))
}

/// What the operator holds at one relay, as that relay last said.
#[derive(Clone, Debug, PartialEq)]
struct Kept {
    relay: String,
    subscriber_key: String,
    filter: Value,
    balance: u64,
    broadcast_price: u64,
}

impl Kept {
    fn json(&self) -> Value {
        json!({
            "relay": self.relay,
            "subscriber_key": self.subscriber_key,
            "filter": self.filter,
            "balance": self.balance,
            "broadcast_price": self.broadcast_price,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            relay: value["relay"].as_str()?.to_owned(),
            subscriber_key: value["subscriber_key"].as_str()?.to_owned(),
            filter: value["filter"].clone(),
            balance: value["balance"].as_u64()?,
            broadcast_price: value["broadcast_price"].as_u64()?,
        })
    }

    /// Read a relay's answer about a subscription, a payment's or a balance's.
    fn from_answer(relay: &str, answer: &str) -> Option<Self> {
        let answer: Value = serde_json::from_str(answer).ok()?;
        Some(Self {
            relay: relay.to_owned(),
            subscriber_key: answer["pubkey"].as_str()?.to_owned(),
            filter: answer["filter"].clone(),
            balance: answer["balance"].as_u64()?,
            broadcast_price: answer["broadcast_price"].as_u64()?,
        })
    }
}

fn kept_path(home: &Path) -> PathBuf {
    home.join("subscriptions.json")
}

fn load(home: &Path) -> Result<Vec<Kept>, Error> {
    let text = match std::fs::read_to_string(kept_path(home)) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error {
                code: ErrorCode::Io,
                message: format!("{}: {source}.", kept_path(home).display()),
            })
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|source| Error {
        code: ErrorCode::Io,
        message: format!("{}: {source}.", kept_path(home).display()),
    })?;
    Ok(value["subscriptions"]
        .as_array()
        .map(|kept| kept.iter().filter_map(Kept::from_json).collect())
        .unwrap_or_default())
}

fn save(home: &Path, kept: &[Kept]) -> Result<(), Error> {
    let value = json!({ "subscriptions": kept.iter().map(Kept::json).collect::<Vec<_>>() });
    node::write(&kept_path(home), value.to_string().as_bytes(), 0o600)
}

/// Keep `now` in place of what was kept for its relay.
fn keep(home: &Path, now: Kept) -> Result<(), Error> {
    let mut kept = load(home)?;
    kept.retain(|other| other.relay != now.relay);
    kept.push(now);
    save(home, &kept)
}

/// `toon relay subscribe`: pay `amount` to `relay`'s subscribe route, as whole packets.
pub fn subscribe(
    home: &Path,
    relay: &str,
    filter: Option<&str>,
    amount: u64,
    yes: bool,
) -> Result<Report, Error> {
    let filter: Option<Value> = filter
        .map(|text| {
            serde_json::from_str(text)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| usage("--filter must be one JSON object, like {\"kinds\":[1]}."))
        })
        .transpose()?;
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let terms = terms(relay)?;
    if filter.is_none() && !load(home)?.iter().any(|kept| kept.relay == relay) {
        return Err(usage(format!(
            "A first subscription to {relay} needs a filter: add --filter."
        )));
    }
    let packets = amount / terms.price;
    if packets == 0 {
        return Err(usage(format!(
            "--amount {amount} is less than {}, the price of one packet to {relay}.",
            terms.price
        )));
    }
    let paid = packets * terms.price;
    if !operator::forwards(home, &terms.address)? {
        return Err(Error {
            code: ErrorCode::PeeringNeeded,
            message: format!(
                "No peering of this agent node reaches {}, where {relay} sells its feed. A \
                 peering is needed, with a deposit of at least {paid} base units: run \
                 `toon peer add {} --deposit {paid}` and then `toon route add {} --peer <id>`.",
                terms.address, terms.connector_url, terms.address
            ),
        });
    }
    if !yes {
        return Err(Error {
            code: ErrorCode::NotConfirmed,
            message: format!(
                "{relay} charges {} per subscribe packet and {} for each event it broadcasts. \
                 {amount} is paid as {packets} packets of {}: {paid} base units, which buy {} \
                 events. Add `--yes` to say that you mean it.",
                terms.price,
                terms.broadcast_price,
                terms.price,
                paid / terms.broadcast_price
            ),
        });
    }

    let secret = subscriber_secret(home)?;
    let url = format!("{}/", event::http_url(relay)?);
    let body = match &filter {
        Some(filter) => json!({ "filter": filter }),
        None => json!({}),
    }
    .to_string()
    .into_bytes();

    spending::spend(home, paid.into(), yes, || {
        let mut paid_so_far = 0;
        let mut credited = 0;
        let mut now: Option<Kept> = None;
        let mut stopped: Option<(&str, Value, String)> = None;
        for _ in 0..packets {
            let headers = vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                (
                    "authorization".to_owned(),
                    authorization(&secret, "POST", &url, Some(&body))?,
                ),
            ];
            let answer = match operator::dispatch_with_headers(
                home,
                &terms.address,
                terms.price,
                &terms.connector_url,
                headers,
                body.clone(),
            ) {
                Ok(answer) => answer,
                Err(error) if paid_so_far == 0 => return Err(error),
                Err(error) => {
                    stopped = Some(("failed", json!({ "message": error.message }), error.message));
                    break;
                }
            };
            match answer {
                Answer::Fulfilled { status, body } => {
                    // A fulfilled packet is paid for, whatever the relay answered.
                    paid_so_far += terms.price;
                    let credit = (200..300).contains(&status).then(|| {
                        let answer: Value = serde_json::from_str(&body).unwrap_or_default();
                        (answer["credited"].as_u64(), Kept::from_answer(relay, &body))
                    });
                    match credit {
                        Some((Some(added), Some(kept))) => {
                            credited += added;
                            now = Some(kept);
                        }
                        _ => {
                            let text =
                                format!("{relay} answered {status} to a subscribe packet: {body}");
                            stopped =
                                Some(("refused", json!({ "status": status, "body": body }), text));
                            break;
                        }
                    }
                }
                Answer::Rejected { code, message } => {
                    let text = format!("A subscribe packet was rejected with {code}. {message}");
                    stopped = Some((
                        "rejected",
                        json!({ "code": code, "message": message }),
                        text,
                    ));
                    break;
                }
                Answer::WrongFulfilment => {
                    stopped = Some((
                        "wrong_fulfilment",
                        Value::Null,
                        "A subscribe packet was fulfilled, but not by the relay's connector."
                            .into(),
                    ));
                    break;
                }
            }
        }
        if let Some(kept) = &now {
            keep(home, kept.clone())?;
        }
        let mut json = json!({
            "relay": relay,
            "packets": packets,
            "paid": paid_so_far,
            "credited": credited,
            "price": terms.price,
        });
        let (exit, text) = match (&now, stopped) {
            (_, None) => {
                let kept = now.as_ref().expect("a packet was credited");
                let events = kept.balance / kept.broadcast_price;
                json["outcome"] = json!("subscribed");
                (
                    Exit::Success,
                    format!(
                        "Subscribed to {relay}: paid {paid_so_far} base units in {packets} \
                         packets and credited {credited}. The balance is {}, which buys {events} \
                         events at {} each.",
                        kept.balance, kept.broadcast_price
                    ),
                )
            }
            (_, Some((outcome, detail, text))) => {
                json["outcome"] = json!(outcome);
                json["response"] = detail;
                (
                    Exit::Failure,
                    format!("{text} Paid {paid_so_far} base units, credited {credited}."),
                )
            }
        };
        if let Some(kept) = &now {
            json["subscriber_key"] = json!(kept.subscriber_key);
            json["balance"] = json!(kept.balance);
            json["broadcast_price"] = json!(kept.broadcast_price);
            json["filter"] = kept.filter.clone();
        }
        Ok((Report { exit, json, text }, paid_so_far > 0))
    })
}

/// A relay's answer to a read of the balance of the subscriber key `secret`.
fn read_balance(relay: &str, secret: &[u8; 32]) -> Option<Kept> {
    let url = format!("{}/", event::http_url(relay).ok()?);
    let response = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .ok()?
        .get(&url)
        .header("accept", "application/toon-subscription+json")
        .header(
            "authorization",
            authorization(secret, "GET", &url, None).ok()?,
        )
        .send()
        .ok()?
        .error_for_status()
        .ok()?;
    Kept::from_answer(relay, &response.text().ok()?)
}

/// `toon relay subscriptions`: the balance and filter at each relay the operator
/// subscribed to, as each relay says now, or as it last said when it does not answer.
pub fn subscriptions(home: &Path) -> Result<Report, Error> {
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let mut kept = load(home)?;
    let secret = if kept.is_empty() {
        None
    } else {
        Some(subscriber_secret(home)?)
    };
    let mut shown = Vec::new();
    let mut lines = Vec::new();
    for entry in &mut kept {
        let fresh = secret
            .as_deref()
            .and_then(|secret| read_balance(&entry.relay, secret));
        let current = fresh.is_some();
        if let Some(fresh) = fresh {
            *entry = fresh;
        }
        let mut item = entry.json();
        item["current"] = json!(current);
        shown.push(item);
        lines.push(format!(
            "{}: balance {}, {} per event, filter {}{}",
            entry.relay,
            entry.balance,
            entry.broadcast_price,
            entry.filter,
            if current { "" } else { " (as last answered)" }
        ));
    }
    save(home, &kept)?;
    let text = if lines.is_empty() {
        "No subscriptions.".to_owned()
    } else {
        lines.join("\n")
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "subscriptions": shown }),
        text,
    })
}
