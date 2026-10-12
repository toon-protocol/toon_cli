//! Pending apps (ADR 0010): what an agent key asked to add that the supervisor has not
//! started, until the operator approves or refuses it.
//!
//! The home holds them as `pending-apps.json`. Each entry is one add, exactly as asked for:
//! the image or URL, the TOON app, the allowances and the variables. An approval adds the
//! app from that record and nothing else, so what the operator was shown is what runs.
//! Pending apps do not expire; revoking a key drops its entries.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::node;
use crate::outcome::{Error, ErrorCode};

const FILE_NAME: &str = "pending-apps.json";

/// The most pending apps one key holds.
pub const MOST_PER_KEY: usize = 8;

/// The most refused apps the home keeps for one key: the oldest are forgotten first.
const MOST_REFUSED_PER_KEY: usize = 8;

fn path(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
        unanswered: None,
    }
}

/// One add an agent key asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub app: String,
    /// The public half of the agent key that asked.
    pub key: String,
    /// What was asked for, as the order carried it: the same object an approval adds from.
    pub ask: Value,
    /// Why the operator refused it, once they have: `Some("")` if they gave no reason.
    pub refused: Option<String>,
}

impl Entry {
    pub fn pending(&self) -> bool {
        self.refused.is_none()
    }

    fn json(&self) -> Value {
        json!({
            "app": self.app,
            "key": self.key,
            "ask": self.ask,
            "refused": self.refused.is_some(),
            "reason": self.refused,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            app: value["app"].as_str()?.to_owned(),
            key: value["key"].as_str()?.to_owned(),
            ask: value["ask"].clone(),
            refused: match value["refused"].as_bool()? {
                true => Some(value["reason"].as_str().unwrap_or_default().to_owned()),
                false => None,
            },
        })
    }

    /// What the operator and the agent key are shown: what was asked for, and which key asked.
    pub fn shown(&self) -> Value {
        let mut shown = json!({
            "name": self.app,
            "status": if self.pending() { "pending" } else { "refused" },
            "key": self.key,
            "to": self.ask["to"],
            "image": self.ask["image"],
            "url": self.ask["url"],
            "address": self.ask["address"],
            "price": self.ask["price"],
            "allowances": {
                "allow_egress": self.ask["allow_egress"],
                "scratch": self.ask["scratch"],
                "memory": self.ask["memory"],
                "cpus": self.ask["cpus"],
                "pids": self.ask["pids"],
            },
            "variables": self.ask["env"],
        });
        if let Some(reason) = &self.refused {
            shown["reason"] = json!((!reason.is_empty()).then_some(reason));
        }
        shown
    }

    /// One line of text for `shown`.
    pub fn line(&self) -> String {
        let what = match (self.ask["image"].as_str(), self.ask["url"].as_str()) {
            (Some(image), _) => format!("image {image}"),
            (_, Some(url)) => format!("the app at {url}"),
            _ => "an app".to_owned(),
        };
        let mut asked: Vec<String> = Vec::new();
        if self.ask["allow_egress"] == true {
            asked.push("egress".into());
        }
        for path in self.ask["scratch"].as_array().into_iter().flatten() {
            asked.push(format!("scratch {}", path.as_str().unwrap_or_default()));
        }
        for field in ["memory", "cpus", "pids"] {
            if let Some(value) = self.ask[field].as_str() {
                asked.push(format!("{field} {value}"));
            }
        }
        for variable in self.ask["env"].as_array().into_iter().flatten() {
            asked.push(format!(
                "variable {}",
                variable.as_str().unwrap_or_default()
            ));
        }
        let with = if asked.is_empty() {
            String::new()
        } else {
            format!(", with {}", asked.join(", "))
        };
        let head = format!(
            "{}: {what} behind {}{with}, asked for by the agent key {}",
            self.app,
            self.ask["to"].as_str().unwrap_or("?"),
            self.key
        );
        match &self.refused {
            None => format!("{head}; waiting for `toon approve` or `toon refuse`."),
            Some(reason) if reason.is_empty() => format!("{head}; refused."),
            Some(reason) => format!("{head}; refused: {reason}"),
        }
    }
}

pub fn load(home: &Path) -> Result<Vec<Entry>, Error> {
    let file = path(home);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(failed(
                ErrorCode::Io,
                format!("{}: {source}.", file.display()),
            ))
        }
    };
    let corrupt = || {
        failed(
            ErrorCode::Io,
            format!(
                "{} is not a record of pending apps this version reads.",
                file.display()
            ),
        )
    };
    let value: Value = serde_json::from_str(&text).map_err(|_| corrupt())?;
    value["apps"]
        .as_array()
        .ok_or_else(corrupt)?
        .iter()
        .map(|entry| Entry::from_json(entry).ok_or_else(corrupt))
        .collect()
}

fn save(home: &Path, entries: &[Entry]) -> Result<(), Error> {
    let value = json!({ "apps": entries.iter().map(Entry::json).collect::<Vec<_>>() });
    node::write(&path(home), format!("{value}\n").as_bytes(), 0o600)
}

/// The pending and refused apps the agent key `key` asked for.
pub fn of_key(home: &Path, key: &str) -> Result<Vec<Entry>, Error> {
    Ok(load(home)?
        .into_iter()
        .filter(|entry| entry.key == key)
        .collect())
}

/// What recording an ask came to.
pub enum Recorded {
    /// A new pending app.
    New,
    /// The same ask is already pending: nothing was recorded.
    Same,
}

/// Record that `key` asked for `ask` as the app `app`. The same ask again records nothing
/// more. A name that another ask holds, or a ninth pending app, is refused. A refused ask of
/// the same key under the name is replaced.
pub fn record(home: &Path, key: &str, app: &str, ask: Value) -> Result<Recorded, Error> {
    let mut entries = load(home)?;
    if let Some(held) = entries.iter().find(|entry| entry.app == app) {
        if held.key == key && held.pending() {
            if held.ask == ask {
                return Ok(Recorded::Same);
            }
            return Err(failed(
                ErrorCode::NameTaken,
                format!(
                    "This key already has a pending app called {app}, asked for differently. \
                     The operator approves or refuses it first."
                ),
            ));
        }
        if held.key != key || held.pending() {
            return Err(failed(
                ErrorCode::NameTaken,
                format!("A pending app is already called {app}."),
            ));
        }
        entries.retain(|entry| entry.app != app);
    }
    let held = entries
        .iter()
        .filter(|entry| entry.key == key && entry.pending())
        .count();
    if held >= MOST_PER_KEY {
        return Err(Error {
            nothing_sent: true,
            ..failed(
                ErrorCode::NeedsOperator,
                format!(
                    "This key already has {MOST_PER_KEY} pending apps, the most it may hold. \
                     The operator must approve or refuse one with `toon approve` or \
                     `toon refuse` first."
                ),
            )
        });
    }
    entries.push(Entry {
        app: app.to_owned(),
        key: key.to_owned(),
        ask,
        refused: None,
    });
    // Only the latest refusals are kept: a key cannot fill the record by being refused.
    let mut refused = entries
        .iter()
        .filter(|entry| entry.key == key && !entry.pending())
        .count();
    entries.retain(|entry| {
        let drop = entry.key == key && !entry.pending() && refused > MOST_REFUSED_PER_KEY;
        if drop {
            refused -= 1;
        }
        !drop
    });
    save(home, &entries)?;
    Ok(Recorded::New)
}

/// The pending app `app`, for the operator to act on.
pub fn find_pending(home: &Path, app: &str) -> Result<Entry, Error> {
    let entries = load(home)?;
    entries
        .iter()
        .find(|entry| entry.app == app && entry.pending())
        .cloned()
        .ok_or_else(|| {
            let waiting: Vec<&str> = entries
                .iter()
                .filter(|entry| entry.pending())
                .map(|entry| entry.app.as_str())
                .collect();
            failed(
                ErrorCode::UnknownName,
                if waiting.is_empty() {
                    format!("No app is pending: {app} is not waiting for the operator.")
                } else {
                    format!(
                        "No app called {app} is pending. These are: {}.",
                        waiting.join(", ")
                    )
                },
            )
        })
}

/// Take the pending app `app` out of the record, once it is added.
pub fn forget(home: &Path, app: &str) -> Result<(), Error> {
    let mut entries = load(home)?;
    entries.retain(|entry| !(entry.app == app && entry.pending()));
    save(home, &entries)
}

/// Record that the operator refused the pending app `app`.
pub fn refuse(home: &Path, app: &str, reason: Option<&str>) -> Result<Entry, Error> {
    let mut entries = load(home)?;
    let entry = entries
        .iter_mut()
        .find(|entry| entry.app == app && entry.pending())
        .ok_or_else(|| find_pending(home, app).expect_err("the app is not pending"))?;
    entry.refused = Some(reason.unwrap_or_default().trim().to_owned());
    let refused = entry.clone();
    save(home, &entries)?;
    Ok(refused)
}

/// Drop everything the agent key `key` asked for: it is revoked.
pub fn drop_key(home: &Path, key: &str) -> Result<(), Error> {
    let mut entries = load(home)?;
    let before = entries.len();
    entries.retain(|entry| entry.key != key);
    if entries.len() == before {
        return Ok(());
    }
    save(home, &entries)
}
