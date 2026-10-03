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

/// Describe the connector at `url`, or this agent node's own (of the TOON app `app`, else
/// the first) when no URL is given.
pub fn run(home: Option<&Path>, url: Option<&str>, app: Option<&str>) -> Result<Report, Error> {
    let (url, egress) = match (url, home) {
        (Some(url), Some(home)) => (url.to_owned(), Egress::of(home)?),
        (Some(url), None) => (url.to_owned(), Egress::direct()),
        (None, Some(home)) => (
            format!("{}/ilp", crate::operator::surface_of(home, app)?.url),
            Egress::of(home)?,
        ),
        (None, None) => return Err(crate::home::resolve().unwrap_err()),
    };
    let description = fetch(&egress, &url)?;
    Ok(Report {
        exit: Exit::Success,
        text: render(&url, &description),
        json: json!({ "description": description }),
    })
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
        line.push_str(if route["request"].is_null() {
            ", no request stated"
        } else {
            ", states a request"
        });
        out.push_str(&line);
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}
