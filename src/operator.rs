//! The connector's operator surface, used the way a remote operator would use it.
//!
//! Reads carry the connector's bearer token. A write is signed with the wallet's
//! operator write key, whose public half the connector's config lists. Both go over
//! loopback to the address the supervisor says its connector listens on.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::control;
use crate::node::{self, ConnectorFiles, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long a read of the operator surface waits for the connector.
const PATIENCE: Duration = Duration::from_secs(30);

/// A running connector's operator surface.
struct Surface {
    /// `http://host:port`, with no trailing slash.
    url: String,
    bearer_token: PathBuf,
    write_key: PathBuf,
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error { code, message }
}

/// The operator surface of the first TOON app's connector, which must be running.
fn surface(home: &Path) -> Result<Surface, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let app = &state.toon_apps[0];
    let address = control::ask(home, "status")
        .and_then(|reply| {
            reply["toon_apps"]
                .as_array()?
                .iter()
                .find(|reported| reported["name"] == app.name.as_str())
                .and_then(|reported| reported["connector"]["address"].as_str().map(str::to_owned))
        })
        .ok_or_else(|| {
            failed(
                ErrorCode::NotRunning,
                "The agent node is not running. Run `toon up`.".into(),
            )
        })?;
    Ok(Surface {
        url: format!("http://{address}"),
        bearer_token: ConnectorFiles::of(home, app.connector).bearer_token,
        write_key: node::operator_key(home),
    })
}

/// A read of the connector's operator surface, with its bearer token.
fn read(surface: &Surface, path: &str) -> Result<Vec<Value>, Error> {
    let token = std::fs::read_to_string(&surface.bearer_token).map_err(|source| {
        failed(
            ErrorCode::Io,
            format!("{}: {source}.", surface.bearer_token.display()),
        )
    })?;
    let url = format!("{}{path}", surface.url);
    reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .and_then(|client| client.get(&url).bearer_auth(token.trim()).send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| failed(ErrorCode::ConnectorFailed, format!("GET {url}: {error}.")))
}

/// `toon route list`: the connector's routing table, as it answers it.
pub fn route_list(home: &Path) -> Result<Report, Error> {
    let routes = read(&surface(home)?, "/routes")?;
    let mut lines: Vec<String> = routes
        .iter()
        .map(|route| {
            format!(
                "{} -> {} (price {})",
                route["prefix"].as_str().unwrap_or_default(),
                route["handler_url"].as_str().unwrap_or_default(),
                route["price"]
            )
        })
        .collect();
    if lines.is_empty() {
        lines.push("The connector has no routes.".into());
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "routes": routes }),
        text: lines.join("\n"),
    })
}

/// A channel's amount as the connector reports it, which is absent while it is opening or
/// when the chain could not be read.
fn amount(value: &Value) -> String {
    match value {
        Value::Null => "unknown".into(),
        other => other.to_string(),
    }
}

fn describe_channel(channel: &Value) -> String {
    format!(
        "{} {} {} {} (counterparty {}, collateral {}, landed {}, watermark {})",
        channel["id"].as_str().unwrap_or_default(),
        channel["chain"].as_str().unwrap_or_default(),
        channel["direction"].as_str().unwrap_or_default(),
        channel["status"].as_str().unwrap_or_default(),
        channel["counterparty"].as_str().unwrap_or_default(),
        amount(&channel["collateral"]),
        amount(&channel["landed"]),
        amount(&channel["watermark"]),
    )
}

/// `toon channel list`: every channel the connector holds, inbound and outbound.
pub fn channel_list(home: &Path) -> Result<Report, Error> {
    let channels = read(&surface(home)?, "/channels")?;
    let mut lines: Vec<String> = channels.iter().map(describe_channel).collect();
    if lines.is_empty() {
        lines.push("The connector has no channels.".into());
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "channels": channels }),
        text: lines.join("\n"),
    })
}

/// How long a signed operator write stays valid, as the connector's own `send` has it.
const SIGNATURE_TTL: u64 = 60;

/// An ed25519 key file as the connector reads one: 32 raw bytes or 64 hex characters.
fn write_keypair(path: &Path) -> Result<ed25519_dalek_v1::Keypair, Error> {
    let unusable = |reason: String| failed(ErrorCode::Io, format!("{}: {reason}.", path.display()));
    let raw = std::fs::read(path).map_err(|source| unusable(source.to_string()))?;
    let bytes: [u8; 32] = if raw.len() == 32 {
        raw.as_slice().try_into().expect("32 bytes")
    } else {
        hex::decode(String::from_utf8_lossy(&raw).trim())
            .ok()
            .and_then(|decoded| decoded.try_into().ok())
            .ok_or_else(|| unusable("the operator write key is not 32 bytes".into()))?
    };
    let secret = ed25519_dalek_v1::SecretKey::from_bytes(&bytes)
        .map_err(|source| unusable(source.to_string()))?;
    let public = ed25519_dalek_v1::PublicKey::from(&secret);
    Ok(ed25519_dalek_v1::Keypair { secret, public })
}

/// A channel write: `POST path` with `body`, signed with the wallet's operator write key.
/// The connector's answer is the channel after the write.
fn channel_write(home: &Path, path: &str, body: &Value) -> Result<Value, Error> {
    let surface = surface(home)?;
    let channel_failed = |message: String| failed(ErrorCode::ChannelFailed, message);
    let keypair = write_keypair(&surface.write_key)?;
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (signature_input, signature, content_digest) = connector_operator::signing::sign_request(
        &keypair,
        "POST",
        path,
        &bytes,
        created,
        Some(created + SIGNATURE_TTL),
    );
    let url = format!("{}{path}", surface.url);
    let mut request = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?
        .post(&url)
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature);
    if !bytes.is_empty() {
        request = request.header("content-type", "application/json");
    }
    let response = request
        .body(bytes)
        .send()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?;
    if !status.is_success() {
        return Err(channel_failed(format!(
            "POST {url} answered {status}: {}",
            text.trim()
        )));
    }
    serde_json::from_str(&text).map_err(|error| {
        channel_failed(format!(
            "POST {url}: the answer was not understood ({error}): {text}"
        ))
    })
}

/// A channel id goes into the path the write is signed for, so it is only ever hex or base58.
fn channel_path(id: &str, action: &str) -> Result<String, Error> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(failed(
            ErrorCode::ChannelFailed,
            format!("'{id}' is not a channel id."),
        ));
    }
    Ok(format!("/channels/{id}/{action}"))
}

fn channel_report(channel: Value, did: &str) -> Report {
    Report {
        exit: Exit::Success,
        text: format!("{did}: {}", describe_channel(&channel)),
        json: json!({ "channel": channel }),
    }
}

/// `toon channel open`: open an outbound channel toward the counterparty whose terms
/// `terms` holds, with `deposit` in it.
pub fn channel_open(
    home: &Path,
    terms: &Path,
    deposit: u128,
    url: Option<&str>,
) -> Result<Report, Error> {
    let contents = std::fs::read_to_string(terms).map_err(|source| {
        failed(
            ErrorCode::ChannelFailed,
            format!("{}: {source}.", terms.display()),
        )
    })?;
    let terms: Value = serde_json::from_str(&contents).map_err(|source| {
        failed(
            ErrorCode::ChannelFailed,
            format!("{}: the terms are not JSON ({source}).", terms.display()),
        )
    })?;
    let mut body = json!({ "terms": terms, "deposit": deposit });
    if let Some(url) = url {
        body["url"] = json!(url);
    }
    let mut channel = channel_write(home, "/channels", &body)?;
    let resumed = channel["resumed"].as_bool().unwrap_or(false);
    let did = if resumed { "Resumed an open" } else { "Opened" };
    channel["resumed"] = json!(resumed);
    Ok(channel_report(channel, did))
}

/// `toon channel fund`: add `amount` to an outbound channel.
pub fn channel_fund(home: &Path, id: &str, amount: u128) -> Result<Report, Error> {
    let channel = channel_write(
        home,
        &channel_path(id, "fund")?,
        &json!({ "amount": amount }),
    )?;
    Ok(channel_report(channel, &format!("Funded with {amount}")))
}

/// `toon channel withdraw`: the connector starts the withdrawal, or finishes it once it
/// is due, and says which.
pub fn channel_withdraw(home: &Path, id: &str) -> Result<Report, Error> {
    let channel = channel_write(home, &channel_path(id, "withdraw")?, &Value::Null)?;
    let step = channel["step"].as_str().unwrap_or("withdraw").to_owned();
    Ok(channel_report(channel, &format!("Withdrawal step {step}")))
}

/// `toon channel land`: land the latest voucher held on an inbound channel.
pub fn channel_land(home: &Path, id: &str) -> Result<Report, Error> {
    let channel = channel_write(home, &channel_path(id, "land")?, &Value::Null)?;
    Ok(channel_report(channel, "Landed"))
}

/// What a packet came to.
enum Answer {
    Fulfilled { status: u64, body: String },
    Rejected { code: String, message: String },
    WrongFulfilment,
}

/// Read the connector's one-line summary of a send. The connector's own `send` verb is
/// what forms, seals, signs and checks the packet, and it reports in text.
fn answer(summary: &str) -> Option<Answer> {
    if summary.starts_with("FULFILL WITH THE WRONG FULFILMENT") {
        return Some(Answer::WrongFulfilment);
    }
    if summary.starts_with("FULFILL -- ") {
        let (_, answered) = summary.split_once("the terminating app answered ")?;
        let (status, body) = answered.split_once(":\n")?;
        return Some(Answer::Fulfilled {
            status: status.parse().ok()?,
            body: body.to_owned(),
        });
    }
    let rejected = summary.strip_prefix("REJECT ")?;
    let (code, rest) = rejected.split_once(" -- ")?;
    let message = rest.split_once('\n').map_or("", |(_, message)| message);
    Some(Answer::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

/// `toon send`: one packet from the operator surface to `destination`, for `amount`.
/// A packet that is not fulfilled is a report, not an error, and exits 1.
pub fn send(home: &Path, destination: &str, amount: u64) -> Result<Report, Error> {
    let surface = surface(home)?;
    let send_failed = |message: String| failed(ErrorCode::SendFailed, message);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| send_failed(format!("The async runtime did not start: {error}.")))?;
    let amount_text = amount.to_string();
    let seal_to = format!("{}/ilp", surface.url);
    let write_key = surface.write_key.to_string_lossy();
    let summary = runtime
        .block_on(connector_cli::run(&[
            "toon send",
            "send",
            "--operator",
            &surface.url,
            "--operator-key",
            &write_key,
            "--to",
            destination,
            "--seal-to",
            &seal_to,
            "--amount",
            &amount_text,
        ]))
        .map_err(|error| send_failed(error.to_string()))
        .and_then(|command| match command {
            connector_cli::Command::Finished { summary } => Ok(summary),
            connector_cli::Command::Serve(_) => {
                Err(send_failed("The connector did not send.".into()))
            }
        })?;
    let answer = answer(&summary).ok_or_else(|| {
        send_failed(format!(
            "The connector's answer was not understood: {summary}"
        ))
    })?;
    let sent = format!("{amount} base units to {destination}");
    Ok(match answer {
        Answer::Fulfilled { status, body } => Report {
            exit: Exit::Success,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "fulfilled",
                "response": { "status": status, "body": body },
            }),
            text: format!("Fulfilled: {sent}. The app answered {status}."),
        },
        Answer::Rejected { code, message } => Report {
            exit: Exit::Failure,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "rejected",
                "reject": { "code": code, "message": message },
            }),
            text: format!("Rejected with {code}: {sent}. {message}"),
        },
        Answer::WrongFulfilment => Report {
            exit: Exit::Failure,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "wrong_fulfilment",
            }),
            text: format!(
                "Fulfilled, but not by this connector: {sent}. The fulfilment did not match."
            ),
        },
    })
}
