//! `toon zk enable` and `toon zk disable`: whether a TOON app's connector is a zk edge (ADR 0016).
//!
//! A zk edge takes packets paid from zk allowances, priced in MINA, as well as from channels
//! (connector ADR 0080). Enabling records the settings in the state, writes the state key the
//! wallet derives for the connector, checks the config with the connector's own validation and
//! only then restarts the connector; disabling takes the settings away and restarts it. The
//! collector and the challenger that have to run beside a zk edge are the operator's to run.

use std::path::Path;

use serde_json::json;

use crate::apps;
use crate::cli::{ZkCommand, ZkEnableArgs};
use crate::derive;
use crate::keystore;
use crate::node::{self, State, ZkEdge};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// What has to run beside a zk edge, which every answer of `toon zk enable` says.
const BESIDE: &str = "Run a collector beside it, or the zk edge is never paid out of the vault, \
     and a challenger (`zk-toon-challenger`), or a payer's dishonest escape keeps money it has \
     already forwarded. Until the zk edge signs mutual-close clearances (connector#1556), its \
     payers leave the vault only by escape.";

/// What the rate is, which an answer with one says.
const SPREAD: &str = "The rate out of MINA is static until the next restart and does not follow \
     the market: declare it below what MINA is worth, by as much as the market may move before \
     you change it.";

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code,
        message,
    }
}

/// `toon zk`.
pub fn run(home: &Path, app: Option<&str>, command: &ZkCommand) -> Result<Report, Error> {
    match command {
        ZkCommand::Enable(args) => enable(home, app, args),
        ZkCommand::Disable { yes } => disable(home, app, *yes),
    }
}

/// The state key text, base58, of a 32-byte secret the wallet derived: the secret with its top
/// two bits cleared, which is below the order of Pallas' scalar field and so a scalar as it
/// stands.
fn state_key(secret: &[u8; 32]) -> Result<(String, String), Error> {
    let mut bytes = *secret;
    bytes[0] &= 0x3f;
    bytes.reverse();
    let corrupt = || {
        failed(
            ErrorCode::KeystoreCorrupt,
            "The wallet derived no usable zk state key.".into(),
        )
    };
    let scalar = connector_zk_note::scalar_from_le_bytes(&bytes).ok_or_else(corrupt)?;
    let text = connector_zk_note::SecKey::new(scalar).to_base58();
    let signer = connector_signer::ZkStateSigner::from_base58(&text).ok_or_else(corrupt)?;
    Ok((text, signer.public_key().into_address()))
}

/// Write the state key the wallet derives for the connector of `toon_app`, and return its
/// public half, which the connector's greeting publishes. The same wallet writes the same key.
fn write_state_key(home: &Path, toon_app: &node::ToonApp) -> Result<String, Error> {
    let passphrase = keystore::passphrase()?;
    let phrase = keystore::open(home, &passphrase)?;
    let mnemonic: bip39::Mnemonic = phrase.parse().map_err(|_| {
        failed(
            ErrorCode::KeystoreCorrupt,
            "The keystore does not hold a valid mnemonic.".into(),
        )
    })?;
    let seed = derive::seed(&mnemonic);
    let secret = derive::zk_state_secret(&*seed, toon_app.connector)
        .map_err(|error| failed(ErrorCode::KeystoreCorrupt, error.0))?;
    let (text, public) = state_key(&secret)?;
    let files = node::ConnectorFiles::of(home, toon_app.connector);
    node::write(&files.zk_state_key, text.as_bytes(), 0o600)?;
    Ok(public)
}

/// Check `changed` with the connector's own validation, before anything restarts. A refusal is
/// a setting the connector will not take. Checking writes the config, so the file is put back
/// as it was either way: nothing is changed until the change is applied.
fn check(home: &Path, changed: &State, toon_app: &node::ToonApp) -> Result<(), Error> {
    let file = node::ConnectorFiles::of(home, toon_app.connector).config;
    let kept = std::fs::read(&file).ok();
    let checked = apps::check(home, changed, &toon_app.name);
    if let Some(bytes) = kept {
        node::write(&file, &bytes, 0o600)?;
    }
    checked.map_err(|error| match error.code {
        ErrorCode::ConnectorFailed => failed(
            ErrorCode::ZkEdgeInvalid,
            format!(
                "{} Nothing was changed.",
                error.message.replacen(
                    "The connector would not accept its config",
                    "The connector refuses these zk edge settings",
                    1
                )
            ),
        ),
        _ => error,
    })
}

fn enable(home: &Path, app: Option<&str>, args: &ZkEnableArgs) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let target = apps::toon_app(&state, app)?.clone();
    let settings = ZkEdge {
        vault: args.vault.clone(),
        network: args.network.clone(),
        graphql_url: args.graphql_url.clone(),
        archive_url: args.archive_url.clone(),
        tiers: args.tiers.clone(),
        zk_allowance_lifetime_secs: args.zk_allowance_lifetime,
        open_budget_cap: args.open_budget_cap,
        subsidy_budget: args.subsidy_budget,
        rate: args.rate,
    };
    let mut changed = state.clone();
    for toon_app in &mut changed.toon_apps {
        if toon_app.name == target.name {
            toon_app.zk_edge = Some(settings.clone());
        }
    }
    // The connector checks that the state key file is there, so it is written first, and
    // removed again if this run wrote it and the change does not go ahead.
    let key_file = node::ConnectorFiles::of(home, target.connector).zk_state_key;
    let new_key = !key_file.exists();
    let undo = || {
        if new_key {
            let _ = std::fs::remove_file(&key_file);
        }
    };
    let state_key = write_state_key(home, &target)?;
    check(home, &changed, &target)
        .and_then(|()| apps::confirm(args.yes, &target.name))
        .inspect_err(|_| undo())?;
    let restarted = apps::apply(home, &state, &changed, &target.name, undo)?;
    let rate = settings.rate.map(|rate| rate.to_string());
    let settles = target.evm.is_some() || target.solana.is_some();
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "enabled": true,
            "toon_app": target.name,
            "vault": settings.vault,
            "network": settings.network,
            "tiers": settings.tiers,
            "open_budget_cap": settings.open_budget_cap,
            "zk_allowance_lifetime": settings.zk_allowance_lifetime_secs,
            "subsidy_budget": settings.subsidy_budget,
            "rate": rate,
            "state_key": state_key,
            "restarted": restarted,
            "beside": BESIDE,
        }),
        text: format!(
            "The connector of {} is a zk edge on the vault {} ({}), signing with the state key \
             {state_key}. {}{} {BESIDE}",
            target.name,
            settings.vault,
            settings.network,
            restarted_text(restarted, &target.name, "starts as a zk edge"),
            match (&rate, settles) {
                (Some(rate), true) =>
                    format!(" Forwards cross out of MINA at {rate} tokens per MINA. {SPREAD}"),
                _ => String::new(),
            },
        ),
    })
}

/// That the connector restarted, or how it starts next if the agent node is not running.
fn restarted_text(now: bool, toon_app: &str, next: &str) -> String {
    if now {
        format!("The connector of {toon_app} restarted.")
    } else {
        format!("The agent node is not running: the connector of {toon_app} {next}.")
    }
}

fn disable(home: &Path, app: Option<&str>, yes: bool) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let target = apps::toon_app(&state, app)?.clone();
    if target.zk_edge.is_none() {
        let edges: Vec<&str> = state
            .toon_apps
            .iter()
            .filter(|toon_app| toon_app.zk_edge.is_some())
            .map(|toon_app| toon_app.name.as_str())
            .collect();
        return Err(failed(
            ErrorCode::ZkEdgeNotEnabled,
            if edges.is_empty() {
                format!(
                    "The connector of {} is not a zk edge, and no other is.",
                    target.name
                )
            } else {
                format!(
                    "The connector of {} is not a zk edge. These are: {}; name one with `--app`.",
                    target.name,
                    edges.join(", ")
                )
            },
        ));
    }
    apps::confirm(yes, &target.name)?;
    let mut changed = state.clone();
    for toon_app in &mut changed.toon_apps {
        if toon_app.name == target.name {
            toon_app.zk_edge = None;
        }
    }
    let restarted = apps::apply(home, &state, &changed, &target.name, || {})?;
    let now = restarted_text(restarted, &target.name, "starts without it");
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "enabled": false,
            "toon_app": target.name,
            "restarted": restarted,
        }),
        text: format!(
            "The connector of {} is no longer a zk edge. {now} Zk allowances still open cannot be \
             paid here any more, and their payers leave the vault by escape, which a challenger \
             still has to watch.",
            target.name
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_key_is_a_mina_key_and_the_same_secret_makes_the_same_one() {
        let (text, public) = state_key(&[0xff; 32]).expect("a key from the largest secret");
        assert!(public.starts_with("B62"), "{public}");
        assert_eq!(state_key(&[0xff; 32]).unwrap(), (text.clone(), public));
        assert_ne!(state_key(&[7; 32]).unwrap().0, text);
    }
}
