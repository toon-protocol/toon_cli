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

/// `toon route list`: the connector's routing table, as it answers it.
pub fn route_list(home: &Path) -> Result<Report, Error> {
    let surface = surface(home)?;
    let token = std::fs::read_to_string(&surface.bearer_token).map_err(|source| {
        failed(
            ErrorCode::Io,
            format!("{}: {source}.", surface.bearer_token.display()),
        )
    })?;
    let url = format!("{}/routes", surface.url);
    let routes: Vec<Value> = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .and_then(|client| client.get(&url).bearer_auth(token.trim()).send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| failed(ErrorCode::ConnectorFailed, format!("GET {url}: {error}.")))?;
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
