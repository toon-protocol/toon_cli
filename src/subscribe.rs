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

use crate::control;
use crate::derive;
use crate::egress::Egress;
use crate::event;
use crate::feed;
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
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// What a relay says it sells its feed for, from its information document.
struct Terms {
    /// Where the relay's connector is reached, for the peering the subscriber needs.
    connector_url: String,
    /// The key a packet is sealed to.
    seal_key: [u8; 65],
    /// The subscribe route.
    address: String,
    /// What one packet costs and credits.
    price: u64,
    /// What the relay debits for each event it broadcasts.
    broadcast_price: u64,
}

fn terms(egress: &Egress, relay: &str) -> Result<Terms, Error> {
    let document = event::information_document(egress, relay)?;
    let subscription = &document["toon_subscription"];
    fn text(value: &Value) -> Option<&str> {
        value.as_str().filter(|text| !text.is_empty())
    }
    let positive = |value: &Value| value.as_u64().filter(|number| *number > 0);
    let edge = event::edge_fields(&document["toon"]).map_err(|missing| {
        event::unpayable(format!(
            "The information document of {relay} does not say where its feed is paid for: \
             its `toon` object needs an `ilp_address`, a `connector_url`, a \
             `connector_seal_key` of 65 bytes of hex and a `price`, and has {missing}."
        ))
    })?;
    match (
        text(&subscription["ilp_address"]),
        positive(&subscription["price"]),
        positive(&subscription["broadcast_price"]),
    ) {
        (Some(address), Some(price), Some(broadcast_price)) => Ok(Terms {
            connector_url: edge.connector_url,
            seal_key: edge.seal_key,
            address: address.to_owned(),
            price,
            broadcast_price,
        }),
        _ => Err(event::unpayable(format!(
            "The information document of {relay} has no `toon_subscription` with an \
             `ilp_address`, a `price` and a `broadcast_price` above 0, so it does not sell \
             its feed."
        ))),
    }
}

/// Where the subscriber key's secret is kept for the supervisor, which has no passphrase
/// to open the wallet with, as it keeps the relay's identity key (ADR 0004).
fn key_path(home: &Path) -> PathBuf {
    home.join("subscriber.key")
}

/// The subscriber key's secret as `subscribe` kept it, if it has.
fn kept_secret(home: &Path) -> Option<[u8; 32]> {
    std::fs::read(key_path(home)).ok()?.try_into().ok()
}

/// What the supervisor reads a feed with: the kept secret, and nothing without one.
pub fn receiving_secret(home: &Path) -> Option<[u8; 32]> {
    kept_secret(home)
}

/// The subscriber key's secret, opened with the passphrase.
fn subscriber_secret(home: &Path) -> Result<zeroize::Zeroizing<[u8; 32]>, Error> {
    derive::subscriber_secret(&*event::wallet_seed(home)?).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
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
pub struct Kept {
    pub relay: String,
    pub subscriber_key: String,
    pub filter: Value,
    pub balance: u64,
    pub broadcast_price: u64,
}

impl Kept {
    /// Whether the balance no longer buys an event: the relay stops the feed.
    pub fn exhausted(&self) -> bool {
        self.balance < self.broadcast_price
    }

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
            // What the events a balance buys are counted with: a price of 0 is no answer.
            broadcast_price: answer["broadcast_price"]
                .as_u64()
                .filter(|price| *price > 0)?,
        })
    }
}

fn kept_path(home: &Path) -> PathBuf {
    home.join("subscriptions.json")
}

pub fn load(home: &Path) -> Result<Vec<Kept>, Error> {
    let text = match std::fs::read_to_string(kept_path(home)) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::Io,
                message: format!("{}: {source}.", kept_path(home).display()),
            })
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
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

/// `toon relay subscribe`: pay `amount` to `relay`'s subscribe route, as whole packets of
/// `packet_amount`, which is the relay's subscribe price unless the operator states more
/// (a connector in between may charge to forward).
pub fn subscribe(
    home: &Path,
    relay: &str,
    filter: Option<&str>,
    following: bool,
    amount: u64,
    packet_amount: Option<u64>,
    yes: bool,
) -> Result<Report, Error> {
    let filter = filter.map(event::parse_filter).transpose()?;
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    // The keys followed now, fixed in the filter until the command is run again.
    let (filter, followed) = if following {
        let (filter, count) = event::following(home, filter)?;
        (Some(filter), Some(count))
    } else {
        (filter, None)
    };
    let terms = terms(&Egress::of(home)?, relay)?;
    // A top-up sends the filter last kept again: the relay may have forgotten an exhausted
    // subscription, and the draft says to send `filter` when that may be so.
    let filter = match filter {
        Some(filter) => filter,
        None => load(home)?
            .into_iter()
            .find(|kept| kept.relay == relay)
            .map(|kept| kept.filter)
            .filter(Value::is_object)
            .ok_or_else(|| {
                usage(format!(
                    "A first subscription to {relay} needs a filter: add --filter."
                ))
            })?,
    };
    let packet_amount = packet_amount.unwrap_or(terms.price);
    if packet_amount < terms.price {
        return Err(usage(format!(
            "--packet-amount {packet_amount} is less than {}, the subscribe price of {relay}: \
             its connector would reject the packet.",
            terms.price
        )));
    }
    let packets = amount / packet_amount;
    if packets == 0 {
        return Err(usage(format!(
            "--amount {amount} is less than {packet_amount}, what one packet to {relay} is \
             sent for."
        )));
    }
    let paid = packets as u128 * packet_amount as u128;
    let credit = packets as u128 * terms.price as u128;
    if !operator::forwards(home, &terms.address)? {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::PeeringNeeded,
            message: format!(
                "No peering of this agent node reaches {}, where {relay} sells its feed. A \
                 peering is needed, with a deposit of at least {paid} base units: run \
                 `{}` and then `toon route add {} --peer <id>`.",
                terms.address,
                operator::peer_add_command(&terms.connector_url, &paid.to_string(), ""),
                terms.address
            ),
        });
    }
    if !yes {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::NotConfirmed,
            message: format!(
                "{relay} charges {} per subscribe packet and {} for each event it broadcasts. \
                 {amount} is paid as {packets} packets of {packet_amount}: {paid} base units{}, \
                 which buy {} events. Add `--yes` to say that you mean it.",
                terms.price,
                terms.broadcast_price,
                if packet_amount == terms.price {
                    String::new()
                } else {
                    format!(", of which {credit} is credited")
                },
                credit / terms.broadcast_price as u128
            ),
        });
    }

    let secret = subscriber_secret(home)?;
    node::write(&key_path(home), &*secret, 0o600)?;
    let url = format!("{}/", event::http_url(relay)?);
    let subscriber_key = derive::nostr_public_key(&secret);
    let body = json!({ "filter": filter }).to_string().into_bytes();

    spending::spend_packets(home, paid, yes, |meter| {
        let mut paid_so_far: u64 = 0;
        // Whether the last packet sent was rejected or wrongly fulfilled, which the
        // watermarks tell the cost of, or failed in a way that may have paid.
        let mut metered = false;
        let mut unanswered = false;
        let mut may_have_paid = false;
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
                packet_amount,
                &terms.seal_key,
                ("POST", "/"),
                headers,
                body.clone(),
            ) {
                Ok(Answer::Unanswered) if paid_so_far == 0 => {
                    return Err(operator::unanswered_cost(
                        meter.moved(u128::from(packet_amount)),
                        None,
                    ));
                }
                Ok(Answer::Unanswered) => {
                    // The packets that were answered were paid for; this one cost what
                    // the channels moved by beyond them.
                    unanswered = true;
                    stopped = Some(("unanswered", Value::Null, operator::unanswered_message()));
                    break;
                }
                Ok(answer) => answer,
                Err(error) if paid_so_far == 0 => return Err(error),
                Err(error) => {
                    may_have_paid = !spending::failed_before_paying(&error);
                    stopped = Some(("failed", json!({ "message": error.message }), error.message));
                    break;
                }
            };
            match answer {
                Answer::Fulfilled { status, body } => {
                    // A fulfilled packet is paid for, whatever the relay answered.
                    paid_so_far += packet_amount;
                    let credit = (200..300).contains(&status).then(|| {
                        let answer: Value = serde_json::from_str(&body).unwrap_or_default();
                        (
                            answer["credited"].as_u64(),
                            Kept::from_answer(relay, &body)
                                .filter(|kept| kept.subscriber_key == subscriber_key),
                        )
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
                    metered = true;
                    let text = format!("A subscribe packet was rejected with {code}. {message}");
                    stopped = Some((
                        "rejected",
                        json!({ "code": code, "message": message }),
                        text,
                    ));
                    break;
                }
                Answer::Unanswered => unreachable!("handled above"),
                Answer::WrongFulfilment => {
                    metered = true;
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
        if metered || unanswered {
            // The last packet moved what the channels moved by beyond the fulfilled
            // packets' amount, which is nothing when the agent node's own connector
            // refused it.
            let sent = u128::from(packet_amount);
            let fulfilled = u128::from(paid_so_far);
            let cost = meter
                .moved(fulfilled + sent)
                .saturating_sub(fulfilled)
                .min(sent);
            paid_so_far += u64::try_from(cost).unwrap_or(packet_amount);
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
            "packet_amount": packet_amount,
        });
        if let Some(count) = followed {
            json["following"] = json!(count);
        }
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
                    ) + &followed.map_or_else(String::new, |count| {
                        format!(
                            " The filter holds {count} followed keys as `authors`, a snapshot \
                             that stays fixed until `toon relay subscribe --following` is run again."
                        )
                    }),
                )
            }
            (_, Some((outcome, detail, text))) => {
                let hint = if outcome == "rejected" && detail["code"] == "F03" {
                    format!(
                        " A connector on the path to {relay} refused a packet of \
                         {packet_amount} base units: state the path's exact cost with \
                         `--packet-amount`."
                    )
                } else {
                    String::new()
                };
                json["outcome"] = json!(outcome);
                json["response"] = detail;
                (
                    Exit::Failure,
                    format!("{text} Paid {paid_so_far} base units, credited {credited}.{hint}"),
                )
            }
        };
        if let Some(kept) = &now {
            json["subscriber_key"] = json!(kept.subscriber_key);
            json["balance"] = json!(kept.balance);
            json["broadcast_price"] = json!(kept.broadcast_price);
            json["filter"] = kept.filter.clone();
        }
        // A packet that failed after it may have left stays counted at its amount.
        let counted = u128::from(paid_so_far)
            + if may_have_paid {
                u128::from(packet_amount)
            } else {
                0
            };
        Ok((Report { exit, json, text }, counted))
    })
}

/// What a relay answered to a read of a balance.
enum Read {
    /// The subscription as the relay holds it now.
    Held(Kept),
    /// `404`: the relay holds no subscription for the key, so the balance is 0.
    NotSubscribed,
    /// No answer that could be read.
    Unanswered,
}

/// A relay's answer to a read of the balance of the subscriber key `secret`.
/// An overlay that is not there is an error, and not a relay that did not answer: nothing
/// is dialled in its place.
fn read_balance(egress: &Egress, relay: &str, secret: &[u8; 32]) -> Result<Read, Error> {
    let Some(response) = balance_response(egress, relay, secret)? else {
        return Ok(Read::Unanswered);
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Read::NotSubscribed);
    }
    Ok(response
        .error_for_status()
        .ok()
        .and_then(|response| response.text().ok())
        .and_then(|text| Kept::from_answer(relay, &text))
        .filter(|kept| kept.subscriber_key == derive::nostr_public_key(secret))
        .map_or(Read::Unanswered, Read::Held))
}

fn balance_response(
    egress: &Egress,
    relay: &str,
    secret: &[u8; 32],
) -> Result<Option<reqwest::blocking::Response>, Error> {
    let Ok(base) = event::http_url(relay) else {
        return Ok(None);
    };
    let url = format!("{base}/");
    let client = egress.relay_client(&url, PATIENCE)?;
    let Ok(authorization) = authorization(secret, "GET", &url, None) else {
        return Ok(None);
    };
    Ok(client
        .get(&url)
        .header("accept", "application/toon-subscription+json")
        .header("authorization", authorization)
        .send()
        .ok())
}

/// `toon relay subscriptions`: the balance and filter at each relay the operator
/// subscribed to, as each relay says now, or as it last said when it does not answer.
pub fn subscriptions(home: &Path) -> Result<Report, Error> {
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let egress = Egress::of(home)?;
    let mut kept = load(home)?;
    let secret = if kept.is_empty() {
        None
    } else {
        Some(subscriber_secret(home)?)
    };
    let mut shown = Vec::new();
    let mut lines = Vec::new();
    for entry in &mut kept {
        let read = match secret.as_deref() {
            Some(secret) => read_balance(&egress, &entry.relay, secret)?,
            None => Read::Unanswered,
        };
        let current = !matches!(read, Read::Unanswered);
        match read {
            Read::Held(fresh) => *entry = fresh,
            // The filter is kept, for the next payment to open the subscription again with.
            Read::NotSubscribed => entry.balance = 0,
            Read::Unanswered => {}
        }
        let mut item = entry.json();
        item["current"] = json!(current);
        item["exhausted"] = json!(entry.exhausted());
        shown.push(item);
        lines.push(format!(
            "{}: balance {}, {} per event, filter {}{}{}",
            entry.relay,
            entry.balance,
            entry.broadcast_price,
            entry.filter,
            if current { "" } else { " (as last answered)" },
            if entry.exhausted() {
                " (exhausted: `toon relay subscribe` tops it up)"
            } else {
                ""
            }
        ));
    }
    save(home, &kept)?;
    let (active, exhausted) = totals(&kept);
    if lines.is_empty() {
        lines.push("No subscriptions.".to_owned());
    }
    lines.push(format!("{active} with a balance, {exhausted} exhausted."));
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "subscriptions": shown,
            "totals": { "active": active, "exhausted": exhausted },
        }),
        text: lines.join("\n"),
    })
}

/// How many of `kept` have a balance and how many are exhausted.
pub fn totals(kept: &[Kept]) -> (usize, usize) {
    let exhausted = kept.iter().filter(|kept| kept.exhausted()).count();
    (kept.len() - exhausted, exhausted)
}

/// Note that the relay of `dialled`, the subscription a feed was dialled for, closed that
/// feed with `payment-required`: its balance is below the broadcast price, whatever was
/// last kept. What is left is not known here, and `toon relay subscriptions` asks the relay.
/// A balance kept since the feed was dialled is a top-up the relay had not seen, and stays.
pub fn mark_exhausted(home: &Path, dialled: &Kept) -> Result<(), Error> {
    let mut kept = load(home)?;
    match kept.iter_mut().find(|kept| kept.relay == dialled.relay) {
        Some(entry) if !entry.exhausted() && entry.balance == dialled.balance => {
            entry.balance = 0;
            save(home, &kept)
        }
        _ => Ok(()),
    }
}

/// `toon event follow`: print the events of the live feed of `relay` as they arrive, one
/// JSON document to a line, for as long as the relay sends them. A feed has no end of its
/// own, so this returns only with the reason it stopped.
pub fn follow(home: &Path, relay: &str) -> Result<Report, Error> {
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let Some(kept) = load(home)?.into_iter().find(|kept| kept.relay == relay) else {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::NotSubscribed,
            message: format!(
                "This agent node holds no subscription at {relay}: `toon relay subscribe` opens one."
            ),
        });
    };
    let secret = match kept_secret(home) {
        Some(secret) => secret,
        None => *subscriber_secret(home)?,
    };
    let proxy = Egress::of(home)?.proxy_for(relay)?;
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut stdout = std::io::stdout().lock();
    let mut unwritten = false;
    let ended = feed::read(relay, &secret, &kept.filter, proxy, &stop, |event| {
        use std::io::Write;
        unwritten = writeln!(stdout, "{event}")
            .and_then(|()| stdout.flush())
            .is_err();
        !unwritten
    });
    let failed = |message: String| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::QueryFailed,
        message,
    };
    Err(match ended {
        _ if unwritten => failed("The events could not be written.".into()),
        feed::Ended::Exhausted(reason) => {
            let _ = mark_exhausted(home, &kept);
            failed(format!(
                "The subscription at {relay} has run out: {reason}. `toon relay subscribe` tops it up."
            ))
        }
        feed::Ended::Closed(reason) => failed(format!("{relay} closed the feed: {reason}")),
        feed::Ended::Dropped(message) => failed(message),
        feed::Ended::Stopped => failed(format!("The feed of {relay} stopped.")),
    })
}

/// The TOON app of this agent node that runs its own relay. None is `unknown_name`.
pub fn own_relay_app(state: &node::State) -> Result<&node::ToonApp, Error> {
    state
        .toon_apps
        .iter()
        .find(|app| {
            app.apps
                .iter()
                .any(|behind| behind.name == node::RELAY && behind.source == node::Source::Relay)
        })
        .ok_or_else(|| Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::UnknownName,
            message: "No TOON app of this agent node has a relay.".into(),
        })
}

/// Where the running relay of the TOON app `app` listens, `host:port`, as the agent node's
/// supervisor reports it. The relay not running is `not_running`.
pub fn own_relay_address(home: &Path, app: &str) -> Result<String, Error> {
    let not_running = || Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::NotRunning,
        message: "The relay is not running: `toon up` starts it.".into(),
    };
    let reply = control::ask(home, "status").ok_or_else(not_running)?;
    reply["toon_apps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|reported| reported["name"] == app)
        .and_then(|reported| reported["apps"].as_array())
        .into_iter()
        .flatten()
        .find(|reported| reported["name"] == node::RELAY)
        .filter(|reported| reported["running"] == true)
        .and_then(|reported| reported["address"].as_str())
        .map(str::to_owned)
        .ok_or_else(not_running)
}

/// `toon relay subscriptions --incoming`: who subscribed to the feed of this agent node's
/// own relay, and what each has left, as the running relay lists them on its write port,
/// which nothing but the operator reaches.
pub fn incoming(home: &Path) -> Result<Report, Error> {
    let state = node::State::load(home)?.ok_or_else(|| node::no_agent_node(home))?;
    let body = own_subscribers(home, selling_relay(&state)?)?;
    let subscribers = body["subscribers"]
        .as_array()
        .ok_or_else(|| unreadable_list("it answered no list".into()))?;
    let with_balance = holding_a_balance(subscribers);
    let mut lines: Vec<String> = subscribers
        .iter()
        .map(|subscriber| {
            format!(
                "{}: balance {}, {} per event, filter {}",
                subscriber["pubkey"].as_str().unwrap_or("?"),
                subscriber["balance"],
                subscriber["broadcast_price"],
                subscriber["filter"],
            )
        })
        .collect();
    if lines.is_empty() {
        lines.push("No one subscribes to this agent node's relay.".to_owned());
    }
    lines.push(format!("Subscriber keys with a balance: {with_balance}."));
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "broadcast_price": body["broadcast_price"],
            "subscribers": subscribers,
            "totals": { "subscribers": with_balance },
        }),
        text: lines.join("\n"),
    })
}

fn unreadable_list(why: String) -> Error {
    relay_error(
        ErrorCode::QueryFailed,
        format!("The relay's list of subscribers could not be read: {why}."),
    )
}

/// How many subscriber keys of `subscribers` hold a balance.
fn holding_a_balance(subscribers: &[Value]) -> usize {
    subscribers
        .iter()
        .filter(|subscriber| subscriber["balance"].as_u64().is_some_and(|b| b > 0))
        .count()
}

/// How many subscriber keys hold a balance at the relay of `state`: none when it sells no
/// live feed, and `None` when it does but the running relay does not say.
pub fn incoming_with_balance(home: &Path, state: &node::State) -> Option<usize> {
    let Ok(app) = selling_relay(state) else {
        return Some(0);
    };
    let body = own_subscribers(home, app).ok()?;
    Some(holding_a_balance(body["subscribers"].as_array()?))
}

fn relay_error(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code,
        message,
    }
}

/// The TOON app of `state` whose relay sells its live feed.
fn selling_relay(state: &node::State) -> Result<&node::ToonApp, Error> {
    let app = own_relay_app(state)?;
    if app.relay.selling().is_none() {
        return Err(relay_error(
            ErrorCode::QueryFailed,
            "This agent node's relay does not sell its live feed: `toon relay price --subscribe \
             <amount> --broadcast <amount>` starts."
                .into(),
        ));
    }
    Ok(app)
}

/// The answer of the running relay of `app` to `GET /subscribers`.
fn own_subscribers(home: &Path, app: &node::ToonApp) -> Result<Value, Error> {
    let address = own_relay_address(home, &app.name).map_err(|mut error| {
        error.message =
            "The relay is not running: `toon up` starts it, and it lists its subscribers.".into();
        error
    })?;
    let url = format!("http://{address}/subscribers");
    reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .no_proxy()
        .build()
        .and_then(|client| client.get(&url).send())
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(|response| response.json::<Value>())
        .map_err(|error| unreadable_list(error.to_string()))
}
