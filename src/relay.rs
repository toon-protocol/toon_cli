//! `toon relay`: how the operator sets the relay without editing a file.
//!
//! What the operator asks for is recorded in `state.json`, as everything is. The relay
//! reads its name, description, expiry behaviour and blocklist when it starts, so a change
//! to them restarts it; the price of a write is on the connector's route, which is
//! rendered when the connector starts, so changing it restarts the connector. So is the
//! subscribe route that a relay selling its live feed has (ADR 0005), at the subscribe
//! price; the broadcast price is the relay's own setting. The
//! supervisor restarts the relay and its connector together, so either change drops the
//! packets the connector holds, and needs `--yes` while the agent node is running.

use std::path::Path;

use serde_json::json;

use crate::apps;
use crate::cli::RelayCommand;
use crate::control;
use crate::node::{self, Expiry, RelaySettings, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};

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

/// The TOON app whose connector fronts the relay, and the relay's settings.
fn relay_of(state: &mut State) -> Result<&mut ToonApp, Error> {
    state
        .toon_apps
        .iter_mut()
        .find(|app| app.apps.iter().any(|behind| behind.name == node::RELAY))
        .ok_or_else(|| Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::UnknownName,
            message: "No TOON app of this agent node has a relay.".into(),
        })
}

/// The price of a write to the relay, on its route.
fn write_price(app: &ToonApp) -> u64 {
    app.apps
        .iter()
        .find(|behind| behind.name == node::RELAY)
        .map_or(node::RELAY_WRITE_PRICE, |behind| behind.price)
}

/// A change restarts a running connector, which drops the packets it holds: refuse it
/// unless the operator said `yes`.
fn confirm(home: &Path, yes: bool, what: &str) -> Result<(), Error> {
    if yes || !control::running(home) {
        return Ok(());
    }
    Err(Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::ConfirmationRequired,
        message: format!(
            "{what} restarts the relay and its connector, and the packets the connector \
             holds are dropped. Nothing was changed: run it again with `--yes`."
        ),
    })
}

fn report(settings: &RelaySettings, price: u64, restarted: bool, changed: bool) -> Report {
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
        price,
        node::RELAY_EPHEMERAL_PRICE,
    );
    match settings.selling() {
        Some((subscribe, broadcast)) => text.push_str(&format!(
            "\nPrice of a subscribe packet: {subscribe}\nPrice of a broadcast event: {broadcast}"
        )),
        None => text.push_str("\nThe live feed is not sold."),
    }
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
                "write": price,
                "ephemeral": node::RELAY_EPHEMERAL_PRICE,
                "subscribe": settings.subscribe_price.filter(|_| settings.selling().is_some()),
                "broadcast": settings.broadcast_price.filter(|_| settings.selling().is_some()),
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
        let app = relay_of(&mut state)?;
        return Ok(report(&app.relay, write_price(app), false, false));
    }
    confirm(home, yes, "Changing the relay's settings")?;
    let before = state.clone();
    let app = relay_of(&mut state)?;
    let toon_app = app.name.clone();
    let settings = &mut app.relay;
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
    let price = write_price(app);
    let settings = app.relay.clone();
    let restarted = apps::apply(home, &before, &state, &toon_app)?;
    Ok(report(&settings, price, restarted, true))
}

/// What `toon relay price` was asked to set. A price of `0` for the subscribe route or
/// the broadcast stops selling the live feed, which has no price of nothing.
#[derive(Debug, Default)]
pub struct Prices {
    pub write: Option<u64>,
    pub subscribe: Option<u64>,
    pub broadcast: Option<u64>,
}

fn usage(message: &str) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Usage,
        message: message.into(),
    }
}

/// `toon relay price`: set the price of a write on the connector's route, and the prices
/// of the live feed: the subscribe route's, on the connector, and the broadcast price, on
/// the relay. A running connector restarts for it, and drops the packets it holds, so that
/// needs `yes`.
pub fn price(home: &Path, prices: &Prices, yes: bool) -> Result<Report, Error> {
    if prices.write.is_none() && prices.subscribe.is_none() && prices.broadcast.is_none() {
        return Err(usage(
            "Nothing to set: give the price of a write, `--subscribe` or `--broadcast`.",
        ));
    }
    let mut state = load(home)?;
    let app = relay_of(&mut state)?;
    let settings = &app.relay;
    let (mut subscribe, mut broadcast) = (
        prices.subscribe.or(settings.subscribe_price),
        prices.broadcast.or(settings.broadcast_price),
    );
    if prices.subscribe == Some(0) || prices.broadcast == Some(0) {
        if prices.subscribe.is_some_and(|price| price > 0)
            || prices.broadcast.is_some_and(|price| price > 0)
        {
            return Err(usage(
                "A price of 0 stops selling the live feed: give 0 for both, or for one.",
            ));
        }
        (subscribe, broadcast) = (None, None);
    }
    if subscribe.is_some() != broadcast.is_some() {
        return Err(usage(
            "The live feed is sold at two prices, one for a subscribe packet and one for a \
             broadcast event: give `--subscribe` and `--broadcast` together.",
        ));
    }
    confirm(home, yes, "A new price")?;
    let before = state.clone();
    let app = relay_of(&mut state)?;
    let toon_app = app.name.clone();
    if let Some(amount) = prices.write {
        for behind in app
            .apps
            .iter_mut()
            .filter(|behind| behind.name == node::RELAY)
        {
            behind.price = amount;
        }
    }
    app.relay.subscribe_price = subscribe;
    app.relay.broadcast_price = broadcast;
    let settings = app.relay.clone();
    let write = write_price(app);
    let restarted = apps::apply(home, &before, &state, &toon_app)?;
    Ok(report(&settings, write, restarted, true))
}

/// `toon relay`.
pub fn run(home: &Path, command: RelayCommand) -> Result<Report, Error> {
    match command {
        RelayCommand::Config(args) => config(home, &args.change(), args.yes),
        RelayCommand::Price {
            amount,
            subscribe,
            broadcast,
            yes,
        } => price(
            home,
            &Prices {
                write: amount,
                subscribe,
                broadcast,
            },
            yes,
        ),
        RelayCommand::Subscribe {
            relay,
            filter,
            following,
            amount,
            packet_amount,
            yes,
        } => crate::subscribe::subscribe(
            home,
            &relay,
            filter.as_deref(),
            following,
            amount,
            packet_amount,
            yes,
        ),
        RelayCommand::Subscriptions { incoming: false } => crate::subscribe::subscriptions(home),
        RelayCommand::Subscriptions { incoming: true } => crate::subscribe::incoming(home),
    }
}
