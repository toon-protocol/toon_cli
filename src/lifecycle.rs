//! `toon create` and `toon destroy`: a second TOON app, and its end.
//!
//! A TOON app is a connector with its own identity and keys and an app behind it
//! (ADR 0002). `create` derives the keys at the next index in the wallet, records the TOON
//! app, asks the supervisor to start its connector, and peers it with the TOON app it was
//! created from in both directions. `destroy` stops and removes one, and refuses while a
//! channel of its holds funds.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::apps::{self, Origin};
use crate::node::{self, App, Reach, Source, State, ToonApp};
use crate::operator::{self, PeerAdd, Surface};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::{control, derive, funding, keystore, spending, wallet};

/// How long a sync waits: the connector starts, and binds to its chain.
const SYNC_PATIENCE: Duration = Duration::from_secs(300);

/// What `toon create` was asked for.
pub struct Create<'a> {
    pub name: &'a str,
    /// The TOON app the new one is created from, and peered with: the first if omitted.
    pub from: Option<&'a str>,
    /// The app behind the new connector.
    pub origin: Origin<'a>,
    pub price: u64,
    pub reach: Reach,
    pub accept_anyone_terms: bool,
    pub listen: &'a str,
    /// What each of the two channels is opened with, or `None` for no peering.
    pub deposit: Option<u128>,
    pub yes: bool,
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
    }
}

/// Ask the supervisor to make what runs match the state.
fn synced(home: &Path) -> Result<(), Error> {
    let reply = control::ask_within(home, "sync", SYNC_PATIENCE);
    match &reply {
        Some(reply) if reply["reloaded"] == true => Ok(()),
        Some(reply) if reply["error"].is_object() => {
            let code = match reply["error"]["code"].as_str() {
                Some("app_failed") => ErrorCode::AppFailed,
                Some("overlay_unavailable") => ErrorCode::OverlayUnavailable,
                _ => ErrorCode::ConnectorFailed,
            };
            Err(failed(
                code,
                reply["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ))
        }
        _ => Err(failed(
            ErrorCode::ConnectorFailed,
            "The supervisor did not start or stop the connector.".into(),
        )),
    }
}

/// Remove what a TOON app has on disk: its connector's keys, config and state, and its
/// apps' data. The connector's directory stays, with a hidden service's onion key, so that
/// `next_connector` does not give its index, and the keys derived at it, to another TOON app.
fn remove_files(home: &Path, app: &ToonApp) {
    let dir = node::ConnectorFiles::of(home, app.connector).config;
    if let Some(dir) = dir.parent() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name() != "onion.key" {
                let path = entry.path();
                let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
            }
        }
    }
    for behind in &app.apps {
        let _ = std::fs::remove_dir_all(node::AppFiles::of(home, &behind.name).data_dir);
        let _ = std::fs::remove_file(node::AppFiles::of(home, &behind.name).identity_key);
    }
}

/// The next index of the wallet's connector keys: past every TOON app's, and past every
/// connector directory a destroyed TOON app left.
fn next_connector(home: &Path, state: &State) -> u32 {
    let left = std::fs::read_dir(home.join("connectors"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok());
    state
        .toon_apps
        .iter()
        .map(|app| app.connector)
        .chain(left)
        .map(|connector| connector + 1)
        .max()
        .unwrap_or(0)
}

/// The URL another connector on this machine peers toward, to reach `app`: its onion
/// endpoint if it is a hidden service, else the address it listens on.
fn peer_url(home: &Path, app: &ToonApp, surface: &Surface) -> Result<String, Error> {
    match &app.reach {
        Reach::Hidden => node::onion_endpoint(home, app)
            .map(|endpoint| format!("http://{endpoint}/ilp"))
            .ok_or_else(|| {
                failed(
                    ErrorCode::PeerFailed,
                    format!("The TOON app {} has no onion endpoint.", app.name),
                )
            }),
        Reach::Clearnet { .. } => Ok(format!("{}/ilp", surface.url)),
    }
}

/// What a peering between two TOON apps is: the connector of `from` peers toward `to` and
/// forwards the packets for `to`'s address to it.
fn peer(
    home: &Path,
    from: (&ToonApp, &Surface),
    to: (&ToonApp, &Surface),
    deposit: u128,
) -> Result<Value, Error> {
    let url = peer_url(home, to.0, to.1)?;
    let peering = operator::peer_add_on(
        from.1,
        &PeerAdd {
            address: &url,
            deposit,
            id: Some(&to.0.name),
            fee: 0,
            max_packet_amount: 0,
        },
    )?;
    let prefix = format!("g.toon.{}", to.0.name);
    operator::route_add_on(from.1, &prefix, &to.0.name, 0)?;
    Ok(json!({
        "from": from.0.name,
        "to": to.0.name,
        "route": prefix,
        "peering": peering.json["peering"],
    }))
}

/// `toon create`.
pub fn create(home: &Path, create: &Create) -> Result<Report, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let source = apps::toon_app(&state, create.from)?;
    apps::free(&state, create.name)?;
    wallet::check_reach(&create.reach, create.accept_anyone_terms, create.listen)?;
    // A peering is made through the connectors, so both must run.
    if create.deposit.is_some() {
        operator::surface_of(home, Some(&source.name))?;
    }
    let passphrase = keystore::passphrase()?;
    let mnemonic = keystore::open(home, &passphrase)?;
    let mnemonic: bip39::Mnemonic = mnemonic.parse().map_err(|_| {
        failed(
            ErrorCode::KeystoreCorrupt,
            "The keystore does not hold a valid mnemonic.".into(),
        )
    })?;
    let seed = derive::seed(&mnemonic);
    let connector = next_connector(home, &state);
    if connector > derive::MAX_CONNECTOR_INDEX {
        return Err(failed(
            ErrorCode::KeystoreCorrupt,
            format!("The wallet has no keys beyond connector {}.", connector - 1),
        ));
    }
    let prefix = format!("g.toon.{}", create.name);
    let new = ToonApp {
        name: create.name.to_owned(),
        connector,
        reach: create.reach.clone(),
        // The port is chosen now, once, so that the address the connector publishes to its
        // peers is the same one every time it starts.
        listen: node::concrete(create.listen)?,
        evm: source.evm.clone(),
        solana: source.solana.clone(),
        plaintext_peers: source.plaintext_peers,
        apps: vec![App {
            name: create.name.to_owned(),
            source: match create.origin {
                Origin::Image(image) => Source::Image(image.to_owned()),
                Origin::Url(url) => Source::Url(url.to_owned()),
            },
            prefix: prefix.clone(),
            price: create.price,
        }],
        relay: node::RelaySettings::default(),
    };
    let from = source.name.clone();
    let make = || made(home, &state, &new, &*seed);
    let Some(deposit) = create.deposit else {
        let restarted = make()?;
        return Ok(created(home, &new, &from, restarted, Vec::new()));
    };
    // The two channels are two payments out of the wallet.
    spending::spend(home, deposit.saturating_mul(2), create.yes, || {
        let restarted = match make() {
            Ok(restarted) => restarted,
            // Nothing was paid.
            Err(error) => return Ok((Err(error), false)),
        };
        let mut peerings = Vec::new();
        let outcome = (|| {
            let state = State::load(home)?.ok_or_else(|| node::no_agent_node(home))?;
            let source = state.toon_apps.iter().find(|app| app.name == from);
            let (Some(source), Some(new)) = (
                source,
                state.toon_apps.iter().find(|app| app.name == new.name),
            ) else {
                return Err(failed(
                    ErrorCode::PeerFailed,
                    "A TOON app is gone from the state.".into(),
                ));
            };
            let near = operator::surface_of(home, Some(&source.name))?;
            let far = operator::surface_of(home, Some(&new.name))?;
            peerings.push(peer(home, (new, &far), (source, &near), deposit)?);
            peerings.push(peer(home, (source, &near), (new, &far), deposit)?);
            Ok(())
        })();
        Ok((
            match outcome {
                Ok(()) => Ok(created(home, &new, &from, restarted, peerings)),
                Err(error) => Err(failed(
                    error.code,
                    format!(
                        "The TOON app {} was created, and {} of its 2 peerings were: {} \
                         `toon peer add --app` and `toon route add --app` make the rest.",
                        new.name,
                        peerings.len(),
                        error.message
                    ),
                )),
            },
            // The first deposit may have gone out.
            true,
        ))
    })?
}

/// Write the new TOON app's keys, record it, and start its connector if the agent node
/// runs. Everything is put back if it fails. Returns whether a connector was started.
fn made(home: &Path, state: &State, new: &ToonApp, seed: &[u8]) -> Result<bool, Error> {
    let undo = |error: Error| {
        let _ = state.save(home);
        remove_files(home, new);
        // Nothing was created, so the index is free for the next `toon create`: the one
        // whose settlement key an `unfunded` failure asked to fund.
        if let Some(dir) = node::ConnectorFiles::of(home, new.connector)
            .config
            .parent()
        {
            let _ = std::fs::remove_dir_all(dir);
        }
        error
    };
    let mut changed = state.clone();
    changed.toon_apps.push(new.clone());
    wallet::write_connector_keys(home, seed, new).map_err(undo)?;
    wallet::write_onion_key(home, seed, new).map_err(undo)?;
    apps::check(home, &changed, &new.name).map_err(undo)?;
    // A connector whose key holds nothing would fail later and not say why. A chain that
    // cannot be asked is not a verdict, as it is not for `toon up`.
    if let Ok(lacking) = funding::shortfalls(funding::needs(home, new).map_err(undo)?) {
        if !lacking.is_empty() {
            let list: Vec<String> = lacking.iter().map(funding::Need::text).collect();
            return Err(undo(failed(
                ErrorCode::Unfunded,
                format!(
                    "The settlement key of the new TOON app is not funded, so nothing was \
                     created. It needs: {}. Fund it, and run `toon create` again.",
                    list.join("; ")
                ),
            )));
        }
    }
    changed.save(home).map_err(undo)?;
    if !control::running(home) {
        return Ok(false);
    }
    synced(home).map_err(|error| {
        let error = undo(error);
        // A connector that came up before the failure is stopped again.
        let _ = synced(home);
        error
    })?;
    Ok(true)
}

fn created(home: &Path, new: &ToonApp, from: &str, started: bool, peerings: Vec<Value>) -> Report {
    let described = json!({
        "name": new.name,
        "connector": new.connector,
        "reach": match &new.reach { Reach::Hidden => "hidden", Reach::Clearnet { .. } => "clearnet" },
        "onion_endpoint": node::onion_endpoint(home, new),
        "listen": new.listen,
        "address": format!("g.toon.{}", new.name),
        "config": node::ConnectorFiles::of(home, new.connector).config,
    });
    let peered = if peerings.is_empty() {
        String::new()
    } else {
        format!(" It is peered with {from} in both directions.")
    };
    let running = if started {
        "Its connector is running."
    } else {
        "The agent node is not running: `toon up` starts its connector."
    };
    Report {
        exit: Exit::Success,
        json: json!({
            "created": described,
            "from": from,
            "started": started,
            "peerings": peerings,
        }),
        text: format!(
            "Created the TOON app {} from {from}, on connector {}.{peered} {running}",
            new.name, new.connector
        ),
    }
}

/// A channel amount as the connector reports it, which is absent while the channel is
/// opening or when the chain could not be read.
fn quantity(value: &Value) -> Option<u128> {
    match value {
        Value::Number(number) => number.as_u64().map(u128::from),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Whether funds are in `channel`: on an outbound channel, collateral that has not been
/// withdrawn; on an inbound one, a voucher that has not landed. An amount the connector
/// could not read counts as funds.
fn holds_funds(channel: &Value) -> bool {
    if channel["direction"] == "outbound" {
        return quantity(&channel["collateral"]).is_none_or(|collateral| collateral > 0);
    }
    match quantity(&channel["watermark"]) {
        Some(watermark) => quantity(&channel["landed"]).is_none_or(|landed| watermark > landed),
        None => true,
    }
}

/// `toon destroy`: stop and remove the TOON app `name`, unless one of its channels holds
/// funds.
pub fn destroy(home: &Path, name: &str) -> Result<Report, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let Some(app) = state.toon_apps.iter().find(|app| app.name == name) else {
        return Err(apps::unknown(&state, name));
    };
    if state.toon_apps.len() == 1 {
        return Err(failed(
            ErrorCode::LastToonApp,
            format!("{name} is the only TOON app of this agent node, which always has one."),
        ));
    }
    // Its channels are read from its connector, so it must run.
    let channels = operator::channels_on(&operator::surface_of(home, Some(name))?)?;
    let holding: Vec<String> = channels
        .iter()
        .filter(|channel| holds_funds(channel))
        .map(operator::describe_channel)
        .collect();
    if !holding.is_empty() {
        return Err(failed(
            ErrorCode::FundsHeld,
            format!(
                "{name} still holds funds in {} channel(s): {}. Withdraw an outbound channel \
                 with `toon channel withdraw`, and land an inbound one with `toon channel \
                 land`, with `--app {name}`.",
                holding.len(),
                holding.join("; ")
            ),
        ));
    }
    let mut changed = state.clone();
    changed.toon_apps.retain(|app| app.name != name);
    changed.save(home)?;
    if let Err(error) = synced(home) {
        state.save(home)?;
        // A connector that was stopped on the way is started again.
        let _ = synced(home);
        return Err(error);
    }
    remove_files(home, app);
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "destroyed": name, "channels": channels.len() }),
        text: format!(
            "Destroyed the TOON app {name}: its connector stopped and its files are gone."
        ),
    })
}
