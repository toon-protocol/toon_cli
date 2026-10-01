//! `toon relay`: how the operator sets the relay without editing a file.
//!
//! What the operator asks for is recorded in `state.json`, as everything is. The relay
//! reads its name, description, expiry behaviour and blocklist when it starts, so a change
//! to them restarts it; the price of a write is on the connector's route, which is
//! rendered when the connector starts, so changing it restarts the connector. The
//! supervisor restarts the relay and its connector together, so either change drops the
//! packets the connector holds, and needs `--yes` while the agent node is running.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::cli::RelayCommand;
use crate::control;
use crate::node::{self, Expiry, RelaySettings, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long a restart gets: the relay has as long to answer its health check as at `up`.
const RESTART_WITHIN: Duration = Duration::from_secs(180);

/// What `toon relay config` was asked to change. Nothing asked is a read.
#[derive(Debug, Default)]
pub struct Change {
    pub name: Option<String>,
    pub description: Option<String>,
    pub expiry: Option<Expiry>,
    /// Keys to add to the blocklist.
    pub block: Vec<String>,
    /// Keys to remove from it.
    pub unblock: Vec<String>,
}

impl Change {
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.expiry.is_none()
            && self.block.is_empty()
            && self.unblock.is_empty()
    }
}

/// A Nostr public key as the blocklist holds it: 64 hex digits, in lower case.
pub fn public_key(text: &str) -> Result<String, String> {
    if text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!("{text:?} is not a public key: 64 hex digits"))
    }
}

fn load(home: &Path) -> Result<State, Error> {
    State::load(home)?.ok_or_else(|| node::no_agent_node(home))
}

/// The settings of the relay, held by the TOON app whose connector fronts it.
fn relay_of(state: &mut State) -> Result<&mut RelaySettings, Error> {
    state
        .toon_apps
        .iter_mut()
        .find(|app| app.apps.iter().any(|name| name == node::RELAY))
        .map(|app| &mut app.relay)
        .ok_or_else(|| Error {
            code: ErrorCode::UnknownName,
            message: "No TOON app of this agent node has a relay.".into(),
        })
}

/// A change restarts a running connector, which drops the packets it holds: refuse it
/// unless the operator said `yes`.
fn confirm(home: &Path, yes: bool, what: &str) -> Result<(), Error> {
    if yes || !control::running(home) {
        return Ok(());
    }
    Err(Error {
        code: ErrorCode::ConfirmationRequired,
        message: format!(
            "{what} restarts the relay and its connector, and the packets the connector \
             holds are dropped. Nothing was changed: run it again with `--yes`."
        ),
    })
}

/// Ask a running supervisor to start the apps and the connector again, and wait until it
/// has. `false` if none is running: the settings are read when `toon up` starts them.
fn restart(home: &Path) -> Result<bool, Error> {
    let Some(ticket) = control::ask(home, "reload").and_then(|reply| reply["ticket"].as_u64())
    else {
        return Ok(false);
    };
    let deadline = Instant::now() + RESTART_WITHIN;
    while Instant::now() < deadline {
        match control::ask(home, "status") {
            Some(reply) if reply["reloads"].as_u64().is_some_and(|done| done >= ticket) => {
                return Ok(true)
            }
            Some(_) => thread::sleep(Duration::from_millis(50)),
            None => {
                return Err(Error {
                    code: ErrorCode::AppFailed,
                    message: "The relay and its connector did not start again, and the \
                              supervisor stopped. \
                              Run `toon up` for the reason."
                        .into(),
                })
            }
        }
    }
    Err(Error {
        code: ErrorCode::AppFailed,
        message: format!(
            "The relay and its connector did not start again within {} seconds.",
            RESTART_WITHIN.as_secs()
        ),
    })
}

fn report(settings: &RelaySettings, restarted: bool, changed: bool) -> Report {
    let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| "(not set)".into());
    let blocklist = if settings.blocklist.is_empty() {
        "(empty)".to_owned()
    } else {
        settings.blocklist.join(", ")
    };
    let mut text = format!(
        "Name: {}\nDescription: {}\nExpiry: {}\nBlocklist: {blocklist}\n\
         Price of a write: {}\nPrice of an ephemeral write: {}",
        shown(&settings.name),
        shown(&settings.description),
        settings.expiry.as_str(),
        settings.price,
        node::RELAY_EPHEMERAL_PRICE,
    );
    if restarted {
        text.push_str("\nThe relay and its connector were restarted.");
    } else if changed {
        text.push_str("\nNo agent node is running: `toon up` starts the relay with these.");
    }
    Report {
        exit: Exit::Success,
        json: json!({
            "relay": {
                "name": settings.name,
                "description": settings.description,
                "expiry": settings.expiry.as_str(),
                "blocklist": settings.blocklist,
            },
            "prices": {
                "write": settings.price,
                "ephemeral": node::RELAY_EPHEMERAL_PRICE,
            },
            "restarted": restarted,
        }),
        text,
    }
}

/// `toon relay config`: show the relay's settings and prices, or change them and restart
/// the relay.
pub fn config(home: &Path, change: &Change, yes: bool) -> Result<Report, Error> {
    let mut state = load(home)?;
    if change.is_empty() {
        return Ok(report(relay_of(&mut state)?, false, false));
    }
    confirm(home, yes, "Changing the relay's settings")?;
    let settings = relay_of(&mut state)?;
    // An empty name or description unsets it.
    let set = |value: &String| Some(value.clone()).filter(|value| !value.is_empty());
    if let Some(name) = &change.name {
        settings.name = set(name);
    }
    if let Some(description) = &change.description {
        settings.description = set(description);
    }
    if let Some(expiry) = change.expiry {
        settings.expiry = expiry;
    }
    for key in &change.block {
        if !settings.blocklist.contains(key) {
            settings.blocklist.push(key.clone());
        }
    }
    // Unblocking wins when one command does both.
    settings
        .blocklist
        .retain(|key| !change.unblock.contains(key));
    let settings = settings.clone();
    state.save(home)?;
    let restarted = restart(home)?;
    Ok(report(&settings, restarted, true))
}

/// `toon relay price`: set the price of a write on the connector's route. A running
/// connector restarts for it, and drops the packets it holds, so that needs `yes`.
pub fn price(home: &Path, amount: u64, yes: bool) -> Result<Report, Error> {
    let mut state = load(home)?;
    confirm(home, yes, "A new price")?;
    let settings = relay_of(&mut state)?;
    settings.price = amount;
    let settings = settings.clone();
    state.save(home)?;
    let restarted = restart(home)?;
    Ok(report(&settings, restarted, true))
}

/// `toon relay`.
pub fn run(home: &Path, command: RelayCommand) -> Result<Report, Error> {
    match command {
        RelayCommand::Config(args) => config(home, &args.change(), args.yes),
        RelayCommand::Price { amount, yes } => price(home, amount, yes),
    }
}
