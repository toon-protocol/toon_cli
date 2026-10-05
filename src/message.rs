//! `toon message send`: a private message (NIP-17) from the agent identity.
//!
//! The message is a kind 14 rumor, signed by nobody, that names every recipient. It is
//! sealed and gift wrapped once for each recipient and once more for the agent identity
//! itself, the sender's copy (NIP-59). Only the wraps are ever written to a relay. Where to
//! send is the operator's choice: a recipient's kind 10050 is not read.

use std::path::Path;

use serde_json::{json, Value};

use crate::cli::MessageCommand;
use crate::control;
use crate::egress::Egress;
use crate::event;
use crate::gift_wrap;
use crate::home;
use crate::inbox;
use crate::node;
use crate::operator;
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::spending;

/// The kind of a private message, the rumor inside the wraps.
const MESSAGE: u64 = 14;

/// What a send was asked to do, checked.
struct Message {
    recipients: Vec<String>,
    content: String,
    tags: Value,
}

fn usage(message: impl Into<String>) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// A public key as hex: 32 bytes, and a point on the curve.
fn public_key(key: &str) -> Option<String> {
    let key = key.to_ascii_lowercase();
    let bytes = hex::decode(&key).ok().filter(|bytes| bytes.len() == 32)?;
    k256::schnorr::VerifyingKey::from_bytes(&bytes).ok()?;
    Some(key)
}

fn is_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl Message {
    fn of(
        recipients: &[String],
        content: &str,
        reply_to: Option<&str>,
        subject: Option<&str>,
    ) -> Result<Message, Error> {
        if recipients.is_empty() {
            return Err(usage("A message needs at least one recipient public key."));
        }
        if content.is_empty() {
            return Err(usage("A message needs a --content that is not empty."));
        }
        let mut keys: Vec<String> = Vec::new();
        for recipient in recipients {
            let key = public_key(recipient).ok_or_else(|| {
                usage(format!(
                    "{recipient} is not a public key: it is 64 hex digits, a point on the curve."
                ))
            })?;
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut tags: Vec<Value> = keys.iter().map(|key| json!(["p", key])).collect();
        if let Some(id) = reply_to {
            if !is_id(id) {
                return Err(usage(format!(
                    "--reply-to {id} is not a message id: it is 64 hex digits."
                )));
            }
            tags.push(json!(["e", id.to_ascii_lowercase(), "", "reply"]));
        }
        if let Some(subject) = subject {
            tags.push(json!(["subject", subject]));
        }
        Ok(Message {
            recipients: keys,
            content: content.to_owned(),
            tags: Value::Array(tags),
        })
    }
}

fn io(message: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::Io,
        message,
    }
}

/// What was made of a message: the rumor, and a wrap for each recipient and for the sender.
struct Sealed {
    rumor: Value,
    /// The recipients' wraps, in the order of the recipients.
    to_recipients: Vec<Value>,
    /// The agent identity's own copy.
    to_sender: Value,
}

fn seal(secret: &[u8; 32], sender: &str, message: &Message) -> Result<Sealed, Error> {
    let now = event::now();
    let rumor = gift_wrap::rumor(sender, now, MESSAGE, message.tags.clone(), &message.content);
    let wrap = |recipient: &str| {
        gift_wrap::wrap(&rumor, secret, recipient, now).map_err(|error| io(error.0))
    };
    Ok(Sealed {
        to_recipients: message
            .recipients
            .iter()
            .map(|recipient| wrap(recipient))
            .collect::<Result<_, _>>()?,
        to_sender: wrap(sender)?,
        rumor,
    })
}

/// Write the sender's copy to the agent node's own relay, which must take it: with no copy
/// the sender could not find the message again, and nothing is sent.
fn write_copy(home: &Path, wrap: &Value) -> Result<(), Error> {
    let report = event::write(home, wrap.clone(), 0).map_err(|mut error| {
        error.nothing_sent = true;
        error.message = format!(
            "The sender's copy could not be written, so nothing was sent. {}",
            error.message
        );
        error
    })?;
    if report.json["outcome"] == "published" {
        return Ok(());
    }
    Err(Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::SendFailed,
        message: format!(
            "The agent node's own relay did not take the sender's copy, so nothing was sent. {}",
            report.text
        ),
    })
}

/// What one wrap's write left in the report.
fn wrap_entry(wrap: &Value, to: &str, relay: &str, written: &Report) -> Value {
    json!({
        "id": wrap["id"],
        "to": to,
        "relay": relay,
        "outcome": written.json["outcome"],
        "event": wrap,
    })
}

/// The report of a send, from what each wrap came to.
fn report(
    sealed: &Sealed,
    message: &Message,
    wraps: Vec<Value>,
    paid: Option<u128>,
    relay: Option<&str>,
) -> Report {
    let sent = wraps.iter().all(|wrap| wrap["outcome"] == "published");
    let id = sealed.rumor["id"].as_str().unwrap_or_default();
    let mut text = if sent {
        format!(
            "Sent message {id} to {} in {} wraps (one is the sender's copy).",
            message.recipients.join(", "),
            wraps.len()
        )
    } else {
        format!("Message {id} was not sent to every recipient.")
    };
    text.push_str(&format!("\n  rumor: {}", sealed.rumor));
    for wrap in &wraps {
        text.push_str(&format!(
            "\n  wrap {} for {} at {}: {}",
            wrap["id"].as_str().unwrap_or_default(),
            wrap["to"].as_str().unwrap_or_default(),
            wrap["relay"].as_str().unwrap_or_default(),
            wrap["outcome"].as_str().unwrap_or_default()
        ));
    }
    match (paid, relay) {
        (Some(paid), Some(relay)) => {
            text.push_str(&format!("\nPaid {paid} base units to {relay}."))
        }
        _ => text.push_str("\nPaid nothing: the agent node's own relay is written free."),
    }
    let wraps_json: Vec<Value> = wraps
        .iter()
        .map(|wrap| {
            json!({
                "id": wrap["id"],
                "to": wrap["to"],
                "relay": wrap["relay"],
                "outcome": wrap["outcome"],
            })
        })
        .collect();
    Report {
        exit: if sent { Exit::Success } else { Exit::Failure },
        json: json!({
            "outcome": if sent { "sent" } else { "failed" },
            "rumor": sealed.rumor,
            "recipients": message.recipients,
            "wraps": wraps_json,
            "paid": paid.unwrap_or(0),
        }),
        text,
    }
}

/// Send to the agent node's own relay: every wrap, as an operator write, which is free.
fn send_here(home: &Path, message: &Message) -> Result<Report, Error> {
    let secret = event::agent_secret(home)?;
    event::keep_agent_secret(home, &secret)?;
    let sender = event::public_key(&secret)?;
    let sealed = seal(&secret, &sender, message)?;
    let relay = own_relay(home)?;

    write_copy(home, &sealed.to_sender)?;
    let mut wraps = vec![wrap_entry(&sealed.to_sender, &sender, &relay, &published())];
    for (recipient, wrap) in message.recipients.iter().zip(&sealed.to_recipients) {
        let written = event::write(home, wrap.clone(), 0)?;
        wraps.push(wrap_entry(wrap, recipient, &relay, &written));
        if written.json["outcome"] != "published" {
            break;
        }
    }
    Ok(report(&sealed, message, wraps, None, None))
}

fn published() -> Report {
    Report {
        exit: Exit::Success,
        json: json!({ "outcome": "published" }),
        text: String::new(),
    }
}

/// Where the agent node's own relay is written, for the report.
fn own_relay(home: &Path) -> Result<String, Error> {
    let state = node::State::load(home)?.ok_or_else(|| node::no_agent_node(home))?;
    Ok(state.toon_apps[0].relay_prefix())
}

/// Send the recipients' wraps to `relay`, paying its write price for each.
fn send_to(home: &Path, message: &Message, relay: &str, yes: bool) -> Result<Report, Error> {
    let edge = event::edge(&Egress::of(home)?, relay)?;
    let price = edge.price;
    let count = message.recipients.len() as u128;
    let total = u128::from(price) * count;
    if !operator::forwards(home, &edge.ilp_address)? {
        return Err(operator::peering_needed(
            home,
            &edge.ilp_address,
            &edge.connector_url,
            format!(
                "No peering of this agent node reaches {}, where {relay} is paid. A peering is \
                 needed: run `{}` and then `toon route add {} --peer <id>`.",
                edge.ilp_address,
                operator::peer_add_command(&edge.connector_url, "<amount>", ""),
                edge.ilp_address
            ),
        ));
    }
    if !yes {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::NotConfirmed,
            message: format!(
                "Sending to {relay} would send {count} wraps at {price} base units each: {total} \
                 base units. Add `--yes` to say that you mean it."
            ),
        });
    }
    let secret = event::agent_secret(home)?;
    event::keep_agent_secret(home, &secret)?;
    let sender = event::public_key(&secret)?;
    let sealed = seal(&secret, &sender, message)?;
    let own = own_relay(home)?;

    spending::spend_packets(home, total, yes, |packets| {
        // Free, and first, but only once the limit has let the payment through: a copy the
        // sender cannot find again is not worth a payment, and a refused send leaves none.
        write_copy(home, &sealed.to_sender)?;
        let mut wraps = vec![wrap_entry(&sealed.to_sender, &sender, &own, &published())];
        let mut paid: u128 = 0;
        for (recipient, wrap) in message.recipients.iter().zip(&sealed.to_recipients) {
            let written = match event::write_to(
                home,
                wrap.clone(),
                &edge.ilp_address,
                price,
                Some(&edge.seal_key),
            ) {
                Ok(written) => written,
                // Once a wrap has been paid for, a failure is not one before paying: it
                // ends the send and the report counts what the wraps before it cost.
                Err(error) if paid > 0 => {
                    paid = packets.moved(total).max(paid);
                    wraps.push(json!({
                        "id": wrap["id"],
                        "to": recipient,
                        "relay": relay,
                        "outcome": error.code.as_str(),
                    }));
                    break;
                }
                Err(error) => return Err(operator::repriced(error, packets.moved(total))),
            };
            let rejected = written.json["outcome"] == "rejected";
            paid = if rejected {
                packets.moved(total).max(paid)
            } else {
                paid + u128::from(price)
            };
            wraps.push(wrap_entry(wrap, recipient, relay, &written));
            if written.json["outcome"] != "published" {
                break;
            }
        }
        Ok((
            report(&sealed, message, wraps, Some(paid), Some(relay)),
            paid,
        ))
    })
}

/// `toon message`.
pub fn run(command: MessageCommand) -> Result<Report, Error> {
    match command {
        MessageCommand::Send {
            recipients,
            content,
            reply_to,
            subject,
            relay,
            yes,
        } => {
            let message = Message::of(
                &recipients,
                &content,
                reply_to.as_deref(),
                subject.as_deref(),
            )?;
            let home = home::resolve()?;
            if node::State::load(&home)?.is_none() {
                return Err(node::no_agent_node(&home));
            }
            match relay {
                Some(relay) => send_to(&home, &message, &relay, yes),
                None => send_here(&home, &message),
            }
        }
        MessageCommand::List { with, since, limit } => list(&with, since, limit),
    }
}

/// `toon message list`: the private messages the supervisor has opened and kept, which
/// needs no passphrase, opens no keystore and sends and pays nothing.
fn list(with: &[String], since: Option<u64>, limit: Option<usize>) -> Result<Report, Error> {
    let mut others: Vec<String> = Vec::new();
    for key in with {
        others.push(public_key(key).ok_or_else(|| {
            usage(format!(
                "{key} is not a public key: it is 64 hex digits, a point on the curve."
            ))
        })?);
    }
    let home = home::resolve()?;
    if node::State::load(&home)?.is_none() {
        return Err(node::no_agent_node(&home));
    }
    let secret = inbox::kept_secret(&home).ok_or_else(|| Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::AgentKeyNotKept,
        message: "The agent identity's secret is not kept in the agent node's home, so no private \
                  message has been opened. `toon event publish` and `toon message send` write it."
            .into(),
    })?;
    let identity = event::public_key(&secret)?;
    let conversation = (!others.is_empty()).then(|| {
        others.push(identity.clone());
        others.sort();
        others.dedup();
        inbox::conversation(&others)
    });

    let mut messages: Vec<Value> = inbox::load(&home)
        .into_iter()
        .filter_map(|rumor| {
            let participants = inbox::participants(&rumor);
            let from = rumor["pubkey"].as_str()?.to_ascii_lowercase();
            let id = rumor["id"].as_str()?.to_owned();
            let created_at = rumor["created_at"].as_u64()?;
            Some(json!({
                "id": id,
                "conversation": inbox::conversation(&participants),
                "participants": participants,
                "sent": from == identity,
                "from": from,
                "created_at": created_at,
                "kind": rumor["kind"],
                "content": rumor["content"],
                "tags": rumor["tags"],
            }))
        })
        .filter(|message| since.is_none_or(|since| message["created_at"].as_u64() >= Some(since)))
        .filter(|message| {
            conversation
                .as_ref()
                .is_none_or(|conversation| message["conversation"] == *conversation)
        })
        .collect();
    let key = |message: &Value| {
        (
            message["created_at"].as_u64().unwrap_or(0),
            message["id"].as_str().unwrap_or_default().to_owned(),
        )
    };
    messages.sort_by_key(key);
    if let Some(limit) = limit {
        messages.drain(..messages.len().saturating_sub(limit));
    }

    let running = control::running(&home);
    let mut text = if messages.is_empty() {
        "No private message is stored.".to_owned()
    } else {
        messages
            .iter()
            .map(|message| {
                format!(
                    "{} {} {} {}: {}",
                    message["created_at"],
                    message["id"].as_str().unwrap_or_default(),
                    if message["sent"] == true {
                        "to"
                    } else {
                        "from"
                    },
                    message["participants"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|key| *key != identity)
                        .collect::<Vec<_>>()
                        .join(","),
                    message["content"].as_str().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if !running {
        text.push_str(
            "\nThe supervisor is not running, so no new message is opened: `toon up` starts it.",
        );
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "messages": messages,
            "supervisor": if running { "running" } else { "not_running" },
        }),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rumor_inside_every_wrap_names_every_recipient() {
        let secrets = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let keys: Vec<String> = secrets
            .iter()
            .map(|secret| event::public_key(secret).unwrap())
            .collect();
        let message = Message::of(&keys[1..], "hello", None, Some("plans")).unwrap();

        let sealed = seal(&secrets[0], &keys[0], &message).unwrap();

        let wraps = sealed.to_recipients.iter().chain([&sealed.to_sender]);
        let secrets = [&secrets[1], &secrets[2], &secrets[0]];
        for (wrap, secret) in wraps.zip(secrets) {
            let opened = gift_wrap::open(wrap, secret).unwrap();
            assert_eq!(opened.rumor, sealed.rumor);
            assert_eq!(opened.sender, keys[0]);
            assert_eq!(opened.rumor["kind"], 14);
            assert_eq!(
                opened.rumor["tags"],
                json!([["p", keys[1]], ["p", keys[2]], ["subject", "plans"]])
            );
        }
    }
}
