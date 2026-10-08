//! `toon describe`: print what a connector offers before it is paid (connector ADR 0050).
//!
//! Every connector answers `GET <connector>/ilp`, free and unauthenticated, with its
//! self-description. Nothing is paid, so nothing is counted against the spending limit.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::egress::Egress;
use crate::outcome::{Error, ErrorCode, Exit, Report};

const PATIENCE: Duration = Duration::from_secs(30);

fn failed(message: String) -> Error {
    Error {
        code: ErrorCode::DescribeFailed,
        message,
        nothing_sent: true,
        unanswered: None,
    }
}

/// Describe the connector at `url`, through this machine's agent node's egress if it has one.
pub fn url(url: &str) -> Result<Report, Error> {
    describe(&Egress::open()?, url.to_owned())
}

/// Describe this agent node's own connector: of the TOON app `app`, else the first.
pub fn own(home: &Path, app: Option<&str>) -> Result<Report, Error> {
    let url = format!("{}/ilp", crate::operator::surface_of(home, app)?.url);
    describe(&Egress::of(home)?, url)
}

fn describe(egress: &Egress, url: String) -> Result<Report, Error> {
    let description = fetch(egress, &url)?;
    Ok(Report {
        exit: Exit::Success,
        text: render(&url, &description),
        json: json!({ "description": description }),
    })
}

/// The addresses the connector at `url` publishes in its self-description: the list
/// `ilpAddresses` and the single `ilpAddress`, either or both.
pub fn published_addresses(egress: &Egress, url: &str) -> Result<Vec<String>, Error> {
    let description = fetch(egress, url)?;
    let mut addresses: Vec<String> = description["ilpAddresses"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(description.get("ilpAddress"))
        .filter_map(Value::as_str)
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
        .collect();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

fn fetch(egress: &Egress, url: &str) -> Result<Value, Error> {
    let response = egress
        .client(url, PATIENCE)?
        .get(url)
        .header("accept", "application/json")
        .send()
        .map_err(|error| failed(format!("{url} did not answer: {error}.")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failed(format!(
            "{url} answered {status}, not a self-description."
        )));
    }
    let description: Value = response
        .json()
        .map_err(|error| failed(format!("{url} did not answer a self-description: {error}.")))?;
    // A self-description is an object that offers routes.
    if !description.is_object() || !description["routes"].is_array() {
        return Err(failed(format!(
            "{url} did not answer a self-description: it has no `routes`."
        )));
    }
    Ok(description)
}

fn scalar(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

/// The members of `value` that are scalars or lists of them, one `key: value` line each.
fn lines(value: &Value, indent: &str, out: &mut String) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, member) in object {
        match member {
            Value::Object(_) => {
                out.push_str(&format!("{indent}{key}:\n"));
                lines(member, &format!("{indent}  "), out);
            }
            // Settlement terms are lists of objects, one per chain: each its own block.
            Value::Array(items) if items.iter().any(Value::is_object) => {
                out.push_str(&format!("{indent}{key}:\n"));
                for item in items {
                    let mut block = String::new();
                    lines(item, &format!("{indent}    "), &mut block);
                    if block.is_empty() {
                        block = format!("{indent}    {}\n", scalar(item));
                    }
                    block.replace_range(..indent.len() + 4, &format!("{indent}  - "));
                    out.push_str(&block);
                }
            }
            Value::Array(items) => {
                let items: Vec<String> = items.iter().map(scalar).collect();
                out.push_str(&format!("{indent}{key}: {}\n", items.join(", ")));
            }
            Value::Null => {}
            _ => out.push_str(&format!("{indent}{key}: {}\n", scalar(member))),
        }
    }
}

fn render(url: &str, description: &Value) -> String {
    let mut out = format!("Connector at {url}\n");
    let mut terms = String::new();
    let mut routes = Vec::new();
    for (key, member) in description.as_object().into_iter().flatten() {
        match key.as_str() {
            "routes" => routes = member.as_array().cloned().unwrap_or_default(),
            _ if member.is_object() => {
                terms.push_str(&format!("{key}:\n"));
                lines(member, "  ", &mut terms);
            }
            _ => lines(&json!({ key: member }), "", &mut terms),
        }
    }
    out.push_str(&terms);
    if routes.is_empty() {
        out.push_str("Routes: none\n");
    } else {
        out.push_str("Routes:\n");
    }
    for route in &routes {
        let mut line = format!(
            "  {}  price {}",
            scalar(&route["prefix"]),
            scalar(&route["price"])
        );
        if !route["pricePerKib"].is_null() {
            line.push_str(&format!(", {} per KiB", scalar(&route["pricePerKib"])));
        }
        if route["request"].is_null() {
            line.push_str(", no request stated\n");
        } else {
            // As JSON, so that what the operator declared is read exactly.
            line.push_str(&format!(
                ", states a request\n    request: {}\n",
                route["request"]
            ));
        }
        out.push_str(&line);
    }
    out.truncate(out.trim_end().len());
    out
}
