//! `toon init` and `toon wallet show`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use serde_json::{json, Value};

use crate::cli::WalletCommand;
use crate::derive::{self, Addresses};
use crate::egress::Egress;
use crate::event;
use crate::funding;
use crate::keystore;
use crate::node::{self, Reach};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::overlay::{self, Edge};
use crate::profile::Profile;
use crate::runner;
use crate::spending;

/// How many connectors a new wallet lists: an agent node starts as one TOON app.
const FIRST_CONNECTORS: u32 = 1;

/// How many connectors the wallet of the agent node at `home` lists: every index a TOON app
/// uses, and the ones below it.
fn connectors(home: &Path) -> Result<u32, Error> {
    Ok(node::State::load(home)?.map_or(FIRST_CONNECTORS, |state| {
        state
            .toon_apps
            .iter()
            .map(|app| app.connector + 1)
            .max()
            .unwrap_or(FIRST_CONNECTORS)
    }))
}

/// Which relay's identity key the first TOON app's relay gets.
const RELAY_INDEX: u32 = 0;

/// Where a new hidden service's address key comes from, when no file holds it already.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Onion {
    /// Derived from the wallet, so that the same mnemonic makes the same address.
    Derived,
    /// Random: a wallet restored from its mnemonic alone does not recover its addresses,
    /// and says so.
    Fresh,
}

fn addresses(mnemonic: &str, connectors: u32) -> Result<Addresses, Error> {
    let mnemonic: bip39::Mnemonic = mnemonic.parse().map_err(|_| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: "The keystore does not hold a valid mnemonic.".into(),
    })?;
    derive::addresses(&*derive::seed(&mnemonic), connectors).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })
}

fn describe(addresses: &Addresses) -> Value {
    let by_chain = |chain: fn(&derive::ConnectorKeys) -> &str| -> Vec<Value> {
        addresses
            .connectors
            .iter()
            .map(|keys| json!({ "connector": keys.index, "address": chain(keys) }))
            .collect()
    };
    json!({
        "chains": {
            "evm": by_chain(|keys| &keys.evm),
            "solana": by_chain(|keys| &keys.solana),
        },
        "connector_identities": addresses
            .connectors
            .iter()
            .map(|keys| json!({ "connector": keys.index, "public_key": keys.identity }))
            .collect::<Vec<_>>(),
        "operator_write_key": addresses.operator_write,
        "agent_identity": addresses.agent_identity,
    })
}

fn listing(wallet: &Value) -> String {
    let mut lines = Vec::new();
    for chain in ["evm", "solana"] {
        for entry in wallet["chains"][chain].as_array().into_iter().flatten() {
            lines.push(format!(
                "{chain} (connector {}): {}",
                entry["connector"],
                entry["address"].as_str().unwrap_or_default()
            ));
        }
    }
    for entry in wallet["connector_identities"]
        .as_array()
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "connector {} identity: {}",
            entry["connector"],
            entry["public_key"].as_str().unwrap_or_default()
        ));
    }
    lines.push(format!(
        "operator write key: {}",
        wallet["operator_write_key"].as_str().unwrap_or_default()
    ));
    lines.push(format!(
        "agent identity: {}",
        wallet["agent_identity"].as_str().unwrap_or_default()
    ));
    lines.join("\n")
}

/// Create the wallet and the first TOON app, once each. A second `init` finds them and
/// changes nothing: it does not even need the passphrase, unless the TOON app is missing
/// and has to be made from the wallet's keys.
pub fn init(home: &Path, options: &node::Options, restore: bool) -> Result<Report, Error> {
    if keystore::exists(home) {
        if restore {
            return Err(Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::Io,
                message: format!(
                    "There is already a wallet at {}: a mnemonic restores into an empty home.",
                    keystore::path(home).display()
                ),
            });
        }
        return existing(home, options);
    }
    if node::State::load(home)?.is_some() {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Io,
            message: format!(
                "{} has an agent node's state but no wallet.",
                node::state_path(home).display()
            ),
        });
    }
    // Before anything is made: a hidden service that cannot be had creates nothing.
    let edge = edge_for(home, options)?;
    let passphrase = keystore::passphrase()?;
    let mnemonic = if restore {
        restoring_mnemonic()?
    } else {
        let entropy = zeroize::Zeroizing::new(keystore::random::<16>()?);
        bip39::Mnemonic::from_entropy(&*entropy).expect("16 bytes is a valid entropy length")
    };
    let onion = if restore {
        Onion::Fresh
    } else {
        Onion::Derived
    };
    let phrase = zeroize::Zeroizing::new(mnemonic.to_string());
    let wallet = describe(&addresses(&phrase, FIRST_CONNECTORS)?);
    // The TOON app is made and checked before the wallet is kept, so that a command line
    // the connector would refuse does not leave a wallet whose mnemonic nobody saw.
    let state = create_toon_app(home, &mnemonic, options, edge.as_deref(), onion);
    release(home, edge.as_deref());
    let state = state?;
    if !keystore::create(home, &passphrase, &phrase)? {
        return existing(home, options);
    }
    // Where it cannot be kept, the next command that opens the keystore keeps it.
    let _ = event::keep_agent_secret_of(home, &phrase);
    if let Err(error) = state.save(home) {
        // The mnemonic has not been shown, so the wallet goes with the TOON app.
        let _ = std::fs::remove_file(keystore::path(home));
        let _ = std::fs::remove_file(event::agent_key_path(home));
        discard_toon_apps(home);
        return Err(error);
    }
    let (needs, funding_text) = funding::requirements(home, &state)?;
    let hidden = state.toon_apps.iter().any(|app| app.reach == Reach::Hidden);
    let changed = restore && hidden;
    let opening = if restore {
        format!(
            "Wallet restored from your mnemonic at {}.",
            keystore::path(home).display()
        )
    } else {
        format!(
            "Wallet created at {}.\n\nYour mnemonic. It is shown this once and no command shows it again; write it down now:\n\n  {}",
            keystore::path(home).display(),
            *phrase
        )
    };
    let changed_text = if changed {
        format!("\n\n{ADDRESSES_CHANGED}")
    } else {
        String::new()
    };
    let text = format!(
        "{opening}\n\n{}\n\n{}{changed_text}\n\n{}",
        listing(&wallet),
        toon_app_text(home, &state, true),
        funding_text
    );
    let mut json = json!({
        "created": true,
        "mnemonic": (!restore).then(|| &*phrase),
        "wallet": wallet,
        "toon_apps": toon_apps(home, &state, true),
        "notes": notes(&state),
        "network": state.network.name(),
        "needs": needs.iter().map(funding::Need::json).collect::<Vec<_>>(),
    });
    if restore {
        json["restored"] = json!(true);
        json["onion_endpoints_changed"] = json!(changed);
    }
    Ok(Report {
        exit: Exit::Success,
        json,
        text,
    })
}

/// Said when a wallet is restored from its mnemonic alone: the mnemonic does not hold the
/// address keys, a backup does.
const ADDRESSES_CHANGED: &str = "The onion endpoints are new: a mnemonic does not hold the \
    address keys, so every address other operators have created peerings toward has changed. \
    `toon wallet backup` and `toon wallet restore` keep them.";

/// The mnemonic `toon init --from-mnemonic` restores, from the file named by
/// `TOON_MNEMONIC_FILE`, else from `TOON_MNEMONIC`: never a flag, so it is not in a process list.
fn restoring_mnemonic() -> Result<bip39::Mnemonic, Error> {
    let usage = |message: String| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Usage,
        message,
    };
    let text = if let Some(file) = std::env::var_os(MNEMONIC_FILE_ENV).filter(|f| !f.is_empty()) {
        zeroize::Zeroizing::new(std::fs::read_to_string(&file).map_err(|source| {
            usage(format!(
                "{MNEMONIC_FILE_ENV} names {}, which cannot be read: {source}.",
                Path::new(&file).display()
            ))
        })?)
    } else if let Some(value) = std::env::var_os(MNEMONIC_ENV) {
        zeroize::Zeroizing::new(
            value
                .into_string()
                .map_err(|_| usage(format!("{MNEMONIC_ENV} is not valid UTF-8.")))?,
        )
    } else {
        return Err(usage(format!(
            "`--from-mnemonic` reads the mnemonic from the file named by {MNEMONIC_FILE_ENV}, \
             or from {MNEMONIC_ENV}. A flag is not accepted, because it would show in a process list."
        )));
    };
    let words = zeroize::Zeroizing::new(text.split_whitespace().collect::<Vec<_>>().join(" "));
    words
        .parse()
        .map_err(|_| usage("The mnemonic is not a valid BIP-39 phrase.".into()))
}

/// The environment variable that holds the mnemonic `init --from-mnemonic` restores.
pub const MNEMONIC_ENV: &str = "TOON_MNEMONIC";
/// The environment variable that names a file holding that mnemonic.
pub const MNEMONIC_FILE_ENV: &str = "TOON_MNEMONIC_FILE";

/// Remove what writing a TOON app's keys left.
fn discard_toon_apps(home: &Path) {
    // A wallet restored from a backup holds its address keys in these directories, and
    // they are the one thing that cannot be made again.
    if keystore::exists(home) {
        for entry in std::fs::read_dir(home.join("connectors"))
            .into_iter()
            .flatten()
            .flatten()
        {
            for file in std::fs::read_dir(entry.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if file.file_name() != "onion.key" {
                    let path = file.path();
                    let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
                }
            }
        }
    } else {
        let _ = std::fs::remove_dir_all(home.join("connectors"));
    }
    let _ = std::fs::remove_dir_all(home.join("apps"));
    crate::anon::stop(home);
    let _ = std::fs::remove_dir_all(home.join("overlay"));
    let _ = std::fs::remove_file(node::operator_key(home));
    let _ = std::fs::remove_file(spending::limits_path(home));
}

/// Write the keys of the first TOON app's connector, and render and check its config.
/// What it wrote is removed again if it fails.
fn create_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
    edge: Option<&dyn Edge>,
    onion: Onion,
) -> Result<node::State, Error> {
    let created = write_toon_app(home, mnemonic, options, edge, onion);
    if created.is_err() {
        discard_toon_apps(home);
    }
    created
}

/// Write the identity key and the settlement keys the wallet derives for the connector of
/// `app`, which only that connector reads. Its onion key is not here: `init` may make that
/// one at random.
pub fn write_connector_keys(home: &Path, seed: &[u8], app: &node::ToonApp) -> Result<(), Error> {
    let files = node::ConnectorFiles::of(home, app.connector);
    let corrupt = |source: derive::DeriveError| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    };
    let identity = derive::identity_secret(seed, app.connector).map_err(corrupt)?;
    let settlement = derive::evm_settlement_secret(seed, app.connector).map_err(corrupt)?;
    node::write(&files.identity_key, &*identity, 0o600)?;
    node::write(&files.settlement_key, &*settlement, 0o600)?;
    if app.solana.is_some() {
        let solana = derive::solana_settlement_secret(seed, app.connector).map_err(corrupt)?;
        node::write(&files.solana_settlement_key, &*solana, 0o600)?;
    }
    Ok(())
}

/// Write the key a new hidden service's onion endpoint is made of, derived from the wallet
/// so that the same mnemonic makes the same endpoint. A key that is there already stays.
pub fn write_onion_key(home: &Path, seed: &[u8], app: &node::ToonApp) -> Result<(), Error> {
    let files = node::ConnectorFiles::of(home, app.connector);
    if app.reach == Reach::Hidden && !files.onion_key.exists() {
        let onion = derive::onion_secret(seed, app.connector).map_err(|source| Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::KeystoreCorrupt,
            message: source.0,
        })?;
        node::write(&files.onion_key, &*onion, 0o600)?;
    }
    Ok(())
}

fn write_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
    edge: Option<&dyn Edge>,
    onion: Onion,
) -> Result<node::State, Error> {
    let seed = derive::seed(mnemonic);
    // The port is chosen now, once, so that the address a connector publishes to its
    // peers is the same one every time it starts.
    let options = node::Options {
        listen: node::concrete(&options.listen)?,
        ..options.clone()
    };
    let segment = derive::address_segment(&*seed, 0).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })?;
    let state = node::State::first(&options, &segment);
    let operator = derive::operator_write_secret(&*seed).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })?;
    node::write(&node::operator_key(home), &*operator, 0o600)?;
    spending::write_limits(home, &*seed, &options.limits)?;
    for app in &state.toon_apps {
        let files = node::ConnectorFiles::of(home, app.connector);
        let corrupt = |source: derive::DeriveError| Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::KeystoreCorrupt,
            message: source.0,
        };
        write_connector_keys(home, &*seed, app)?;
        if app.apps.iter().any(|app| app.source == node::Source::Relay) {
            // The relay's identity key is the wallet's, handed over as a file that only
            // this user reads. `up` reads it and gives it to the relay.
            let relay = derive::relay_identity_secret(&*seed, RELAY_INDEX).map_err(corrupt)?;
            node::write(
                &node::AppFiles::of(home, node::RELAY).identity_key,
                &*relay,
                0o600,
            )?;
        }
        // Nothing runs yet, so the route is checked against the address the relay's
        // container serves on.
        let placeholder = SocketAddr::from((Ipv4Addr::LOCALHOST, runner::WRITE_PORT));
        let overlay = match (&app.reach, edge) {
            (Reach::Hidden, Some(edge)) => {
                // A key that is there already was restored from a backup: it is kept.
                if !files.onion_key.exists() {
                    let key = match onion {
                        Onion::Derived => {
                            derive::onion_secret(&*seed, app.connector).map_err(corrupt)?
                        }
                        Onion::Fresh => zeroize::Zeroizing::new(keystore::random::<32>()?),
                    };
                    node::write(&files.onion_key, &*key, 0o600)?;
                }
                Some(node::Overlay {
                    proxy: edge.proxy(),
                    endpoint: edge.issue(app.connector, &files.onion_key)?,
                })
            }
            _ => None,
        };
        node::render(
            home,
            app,
            &[(node::RELAY.to_owned(), placeholder)],
            overlay.as_ref(),
        )?;
    }
    Ok(state)
}

/// The wallet was there already. If the TOON app is not, make it from the wallet.
fn existing(home: &Path, options: &node::Options) -> Result<Report, Error> {
    let file = keystore::path(home);
    let (state, created) = match node::State::load(home)? {
        Some(state) => (state, false),
        None => {
            let edge = edge_for(home, options)?;
            let passphrase = keystore::passphrase()?;
            let mnemonic: bip39::Mnemonic =
                keystore::open(home, &passphrase)?
                    .parse()
                    .map_err(|_| Error {
                        nothing_sent: false,
                        unanswered: None,
                        code: ErrorCode::KeystoreCorrupt,
                        message: "The keystore does not hold a valid mnemonic.".into(),
                    })?;
            let state = create_toon_app(home, &mnemonic, options, edge.as_deref(), Onion::Derived);
            release(home, edge.as_deref());
            let state = state?;
            if let Err(error) = state.save(home) {
                discard_toon_apps(home);
                return Err(error);
            }
            (state, true)
        }
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "created": false,
            "keystore": file,
            "toon_apps": toon_apps(home, &state, created),
            "notes": notes(&state),
        }),
        text: format!(
            "A wallet already exists at {}. {}",
            file.display(),
            toon_app_text(home, &state, created)
        ),
    })
}

fn toon_apps(home: &Path, state: &node::State, created: bool) -> Vec<Value> {
    state
        .toon_apps
        .iter()
        .map(|app| {
            let mut described = json!({
                "name": app.name,
                "created": created,
                "config": node::ConnectorFiles::of(home, app.connector).config,
            });
            match &app.reach {
                Reach::Hidden => {
                    described["reach"] = json!("hidden");
                    described["onion_endpoint"] = json!(node::onion_endpoint(home, app));
                    described["ports"] = json!({
                        "connector": overlay::CONNECTOR_PORT,
                        "relay_read": app
                            .apps
                            .iter()
                            .any(|behind| behind.name == node::RELAY)
                            .then_some(overlay::RELAY_READ_PORT),
                    });
                }
                Reach::Clearnet { hostname } => {
                    described["reach"] = json!("clearnet");
                    described["hostname"] = json!(hostname);
                    described["listen"] = json!(app.listen);
                }
            }
            described
        })
        .collect()
}

/// What the operator is told about how each TOON app is reached.
fn reach_text(home: &Path, state: &node::State) -> String {
    state
        .toon_apps
        .iter()
        .map(|app| match &app.reach {
            Reach::Hidden => format!(
                "{} is a hidden service. Its onion endpoint is {}: the connector answers on port {}{}.",
                app.name,
                node::onion_endpoint(home, app).unwrap_or_default(),
                overlay::CONNECTOR_PORT,
                if app.apps.iter().any(|behind| behind.name == node::RELAY) {
                    format!(", and the relay's read port on port {}", overlay::RELAY_READ_PORT)
                } else {
                    String::new()
                },
            ),
            Reach::Clearnet { hostname } => format!(
                "{} is clearnet, as you asked, for {hostname}. Its connector listens on {}. \
                 The certificate and the reverse proxy that answer at {hostname} are yours to provide.",
                app.name, app.listen
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a hidden service does and does not do, said whenever one exists.
const HIDDEN_NOTE: &str =
    "A hidden service hides where the TOON app is reachable, and not who it pays: \
     payments are on a public chain.";

/// Said when the network has no connector: `mainnet`, unless `init` was given one.
const NO_NETWORK_NOTE: &str = "There is no mainnet TOON network yet, so `toon join mainnet` is refused until `init` is given `--connector-url` (and `--relay-url`).";

/// Said when a hidden agent node is initialised for the sandbox without a connector, and
/// when `toon join sandbox` is refused on it.
pub const SANDBOX_HUB_NOTE: &str = "A hidden agent node cannot reach the sandbox's hub on `localhost`, so this one records no connector and `toon join sandbox` is refused. Name the hub with `--connector-url http://<hub>.anyone:3200/ilp` (and `--relay-url ws://<hub>.anyone:7100`) on `init`, or run the agent node with `--clearnet`.";

fn notes(state: &node::State) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if state.toon_apps.iter().any(|app| app.reach == Reach::Hidden) {
        notes.push(HIDDEN_NOTE);
    }
    if state.connector_url.is_none() {
        notes.push(if state.network == Profile::Sandbox {
            SANDBOX_HUB_NOTE
        } else {
            NO_NETWORK_NOTE
        });
    }
    notes
}

fn toon_app_text(home: &Path, state: &node::State, created: bool) -> String {
    let names: Vec<&str> = state
        .toon_apps
        .iter()
        .map(|app| app.name.as_str())
        .collect();
    let reach = reach_text(home, state);
    let note = notes(state).join(" ");
    let note = if note.is_empty() {
        note
    } else {
        format!("\n{note}")
    };
    if created {
        format!(
            "TOON app created: {}. Fund its settlement keys, then run `toon up` to start it.\n{reach}{note}",
            names.join(", ")
        )
    } else {
        format!("Nothing was changed.\n{reach}{note}")
    }
}

/// The overlay a hidden service is made on, bootstrapped; `None` for clearnet, which
/// needs none. A hidden service is not made without the operator's agreement to Anyone's
/// terms, nor without an overlay: this fails before anything is written.
fn edge_for(home: &Path, options: &node::Options) -> Result<Option<Box<dyn Edge>>, Error> {
    check_reach(&options.reach, options.accept_anyone_terms, &options.listen)?;
    match options.reach {
        Reach::Clearnet { .. } => Ok(None),
        Reach::Hidden => overlay::bootstrap(home, true).map(Some),
    }
}

/// Whether a TOON app can be reached as `reach` asks, listening where `listen` says: a
/// clearnet hostname that is one, and a hidden service that the operator agreed to the
/// terms of, which listens on loopback only.
pub fn check_reach(reach: &Reach, accept_anyone_terms: bool, listen: &str) -> Result<(), Error> {
    if let Reach::Clearnet { hostname } = reach {
        if hostname.is_empty()
            || hostname.contains(|c: char| c.is_whitespace() || "/:@".contains(c))
        {
            return Err(Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::Usage,
                message: format!("`--clearnet` takes a hostname, and {hostname:?} is not one."),
            });
        }
        return Ok(());
    }
    if !accept_anyone_terms {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Usage,
            message: "A new TOON app is a hidden service on the Anyone overlay. Pass \
                      `--accept-anyone-terms` to agree to the Anyone Protocol's terms, or \
                      `--clearnet <hostname>` to ask for clearnet instead."
                .into(),
        });
    }
    if listen
        .parse::<SocketAddr>()
        .is_ok_and(|listen| !listen.ip().is_loopback())
    {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Usage,
            message: format!(
                "A hidden service listens on loopback only, and {listen} is not: its onion endpoint \
                 is the only address it is reached at. `--clearnet <hostname>` binds an address \
                 for a hostname instead."
            ),
        });
    }
    Ok(())
}

/// Let the overlay go once the TOON app is made, unless a supervisor is running on it: the
/// daemon is started again by `toon up`.
fn release(home: &Path, edge: Option<&dyn Edge>) {
    if let Some(edge) = edge {
        if !home.join("supervisor.sock").exists() {
            edge.release();
        }
    }
}

/// List the wallet's addresses.
pub fn show(home: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let passphrase = keystore::passphrase()?;
    let mnemonic = keystore::open(home, &passphrase)?;
    let wallet = describe(&addresses(&mnemonic, connectors(home)?)?);
    Ok(Report {
        exit: Exit::Success,
        text: listing(&wallet),
        json: json!({ "wallet": wallet }),
    })
}

/// How long a read of a chain's JSON-RPC endpoint waits for it.
const CHAIN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// The ERC-20 `balanceOf(address)` selector.
const BALANCE_OF: &str = "70a08231";

/// One JSON-RPC call to `rpc_url`, whose `result` is whatever the method answers.
fn call(egress: &Egress, rpc_url: &str, method: &str, params: Value) -> Result<Value, Error> {
    let reply: Value = egress
        .client(rpc_url, CHAIN_PATIENCE)?
        .post(rpc_url)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| chain_failed(rpc_url, method, error.to_string()))?;
    if let Some(error) = reply.get("error") {
        return Err(chain_failed(
            rpc_url,
            method,
            format!("the chain refused: {error}"),
        ));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| chain_failed(rpc_url, method, "the answer has no result".into()))
}

/// `method` to `rpc_url` failed, or answered something this cannot read.
fn chain_failed(rpc_url: &str, method: &str, message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::ChainFailed,
        message: format!("{method} to {rpc_url}: {message}."),
    }
}

/// One JSON-RPC call to `rpc_url`, whose `result` is a hex quantity.
fn quantity(egress: &Egress, rpc_url: &str, method: &str, params: Value) -> Result<u128, Error> {
    let reply = call(egress, rpc_url, method, params)?;
    let result = reply
        .as_str()
        .ok_or_else(|| chain_failed(rpc_url, method, "the answer is not a quantity".into()))?;
    // A 32-byte word. A balance that does not fit its low 16 bytes is refused, not cut.
    let digits = result.trim_start_matches("0x").trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(digits, 16).map_err(|_| {
        chain_failed(
            rpc_url,
            method,
            format!("'{result}' is not a balance this can show"),
        )
    })
}

/// The lamports `address` holds, and its balance of `mint` across its token accounts. An
/// address with no token account holds none of the token.
fn solana_balances(
    egress: &Egress,
    solana: &node::Solana,
    address: &str,
) -> Result<(u64, u128), Error> {
    let native = call(egress, &solana.rpc_url, "getBalance", json!([address]))?;
    let native = native["value"].as_u64().ok_or_else(|| {
        chain_failed(
            &solana.rpc_url,
            "getBalance",
            "the answer has no balance".into(),
        )
    })?;
    let accounts = call(
        egress,
        &solana.rpc_url,
        "getTokenAccountsByOwner",
        json!([address, { "mint": solana.token }, { "encoding": "jsonParsed" }]),
    )?;
    let unreadable = || {
        chain_failed(
            &solana.rpc_url,
            "getTokenAccountsByOwner",
            "the answer has no token balance".into(),
        )
    };
    let mut token: u128 = 0;
    for account in accounts["value"].as_array().ok_or_else(unreadable)? {
        let amount = account["account"]["data"]["parsed"]["info"]["tokenAmount"]["amount"]
            .as_str()
            .and_then(|amount| amount.parse::<u128>().ok())
            .ok_or_else(unreadable)?;
        token = token.checked_add(amount).ok_or_else(unreadable)?;
    }
    Ok((native, token))
}

/// `toon wallet balances`: the balance of every address, by TOON app and chain. An address
/// on a chain the TOON app has no endpoint for has no balance to read, and says so.
pub fn balances(home: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let Some(state) = node::State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let passphrase = keystore::passphrase()?;
    let mnemonic = keystore::open(home, &passphrase)?;
    let addresses = addresses(&mnemonic, connectors(home)?)?;
    let egress = Egress::of_state(home, &state);
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    for app in &state.toon_apps {
        let keys = addresses
            .connectors
            .iter()
            .find(|keys| keys.index == app.connector)
            .ok_or_else(|| Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::KeystoreCorrupt,
                message: format!("The wallet has no keys for connector {}.", app.connector),
            })?;
        let evm = match &app.evm {
            Some(evm) => {
                let native = quantity(
                    &egress,
                    &evm.rpc_url,
                    "eth_getBalance",
                    json!([keys.evm, "latest"]),
                )?;
                let token = quantity(
                    &egress,
                    &evm.rpc_url,
                    "eth_call",
                    json!([{
                        "to": evm.token,
                        "data": format!(
                            "0x{BALANCE_OF}{:0>64}",
                            keys.evm.trim_start_matches("0x").to_lowercase()
                        ),
                    }, "latest"]),
                )?;
                lines.push(format!(
                    "{} evm {}: {native} native, {token} of token {} ({} decimals)",
                    app.name, keys.evm, evm.token, evm.decimals
                ));
                json!({
                    "toon_app": app.name,
                    "chain": "evm",
                    "address": keys.evm,
                    "native": native.to_string(),
                    "token": {
                        "address": evm.token,
                        "decimals": evm.decimals,
                        "balance": token.to_string(),
                    },
                })
            }
            None => {
                lines.push(format!(
                    "{} evm {}: no chain configured",
                    app.name, keys.evm
                ));
                json!({
                    "toon_app": app.name,
                    "chain": "evm",
                    "address": keys.evm,
                    "native": null,
                    "token": null,
                })
            }
        };
        entries.push(evm);
        let solana = match &app.solana {
            Some(solana) => {
                let (native, token) = solana_balances(&egress, solana, &keys.solana)?;
                lines.push(format!(
                    "{} solana {}: {native} native, {token} of token {} ({} decimals)",
                    app.name, keys.solana, solana.token, solana.decimals
                ));
                json!({
                    "toon_app": app.name,
                    "chain": "solana",
                    "address": keys.solana,
                    "native": native.to_string(),
                    "token": {
                        "address": solana.token,
                        "decimals": solana.decimals,
                        "balance": token.to_string(),
                    },
                })
            }
            None => {
                lines.push(format!(
                    "{} solana {}: no chain configured",
                    app.name, keys.solana
                ));
                json!({
                    "toon_app": app.name,
                    "chain": "solana",
                    "address": keys.solana,
                    "native": null,
                    "token": null,
                })
            }
        };
        entries.push(solana);
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "balances": entries }),
        text: lines.join("\n"),
    })
}

/// The version of a backup's contents before they are sealed: the mnemonic the keystore
/// holds and the key of every onion endpoint, by connector.
const BACKUP_VERSION: u64 = 1;

/// `toon wallet backup`: seal the keystore's mnemonic and every address key into one
/// file, under the wallet's passphrase. The file is only written where there is none.
pub fn backup(home: &Path, out: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let Some(state) = node::State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let passphrase = keystore::passphrase()?;
    let mnemonic = keystore::open(home, &passphrase)?;
    let mut onion_keys = serde_json::Map::new();
    let mut endpoints = Vec::new();
    for app in state
        .toon_apps
        .iter()
        .filter(|app| app.reach == Reach::Hidden)
    {
        let file = node::ConnectorFiles::of(home, app.connector).onion_key;
        let key = std::fs::read(&file).map_err(|source| Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Io,
            message: format!("{}: {source}.", file.display()),
        })?;
        let key = zeroize::Zeroizing::new(key);
        onion_keys.insert(app.connector.to_string(), json!(hex::encode(&*key)));
        endpoints.push(json!({
            "toon_app": app.name,
            "connector": app.connector,
            "onion_endpoint": node::onion_endpoint(home, app),
        }));
    }
    let plain = zeroize::Zeroizing::new(
        json!({
            "version": BACKUP_VERSION,
            "mnemonic": &*mnemonic,
            "onion_keys": onion_keys,
        })
        .to_string(),
    );
    let sealed = keystore::seal(&passphrase, plain.as_bytes())?;
    let io = |source: std::io::Error| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Io,
        message: format!("{}: {source}.", out.display()),
    };
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(out)
            .map_err(io)?;
        let written = writeln!(file, "{sealed:#}").and_then(|()| file.sync_all());
        if let Err(source) = written {
            let _ = std::fs::remove_file(out);
            return Err(io(source));
        }
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "backup": out, "onion_endpoints": endpoints }),
        text: format!(
            "Backup written to {}. It holds the mnemonic and {} address key(s), sealed under \
             the wallet's passphrase: keep both somewhere other than this machine.",
            out.display(),
            endpoints.len()
        ),
    })
}

/// `toon wallet restore`: make the wallet and the address keys from a backup, in a home
/// with no wallet. The keystore is sealed under the passphrase that opened the backup.
/// `toon init` then makes the TOON app on the keys it finds, at the same onion endpoints.
pub fn restore(home: &Path, from: &Path) -> Result<Report, Error> {
    if keystore::exists(home) {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Io,
            message: format!(
                "There is already a wallet at {}: a backup restores into an empty home.",
                keystore::path(home).display()
            ),
        });
    }
    if node::State::load(home)?.is_some() {
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Io,
            message: format!(
                "{} has an agent node's state but no wallet: a backup restores into an empty home.",
                node::state_path(home).display()
            ),
        });
    }
    let passphrase = keystore::passphrase()?;
    let text = std::fs::read_to_string(from).map_err(|source| Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Io,
        message: format!("{}: {source}.", from.display()),
    })?;
    let corrupt = || Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::KeystoreCorrupt,
        message: format!("{} is not a backup this version reads.", from.display()),
    };
    let plain =
        zeroize::Zeroizing::new(keystore::unseal(&text, &passphrase, from).map_err(|error| {
            match error.code {
                ErrorCode::KeystoreCorrupt => corrupt(),
                _ => error,
            }
        })?);
    let document: Value = serde_json::from_slice(&plain).map_err(|_| corrupt())?;
    if document["version"] != BACKUP_VERSION {
        return Err(corrupt());
    }
    let phrase = zeroize::Zeroizing::new(
        document["mnemonic"]
            .as_str()
            .ok_or_else(corrupt)?
            .to_owned(),
    );
    let wallet = describe(&addresses(&phrase, FIRST_CONNECTORS)?);
    let mut keys = Vec::new();
    for (connector, key) in document["onion_keys"].as_object().ok_or_else(corrupt)? {
        let connector: u32 = connector.parse().map_err(|_| corrupt())?;
        let key = zeroize::Zeroizing::new(
            hex::decode(key.as_str().ok_or_else(corrupt)?).map_err(|_| corrupt())?,
        );
        if key.len() != 32 {
            return Err(corrupt());
        }
        keys.push((connector, key));
    }
    // The address keys first: if one cannot be written there is no wallet that lacks them.
    let mut endpoints = Vec::new();
    let mut lines = Vec::new();
    let mut written = Vec::new();
    // Only the files this restore wrote are removed if it fails, so that nothing else in the
    // home is touched.
    let undo = |written: &[std::path::PathBuf]| {
        for file in written {
            let _ = std::fs::remove_file(file);
        }
    };
    for (connector, key) in &keys {
        let file = node::ConnectorFiles::of(home, *connector).onion_key;
        if file.exists() {
            undo(&written);
            return Err(Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::Io,
                message: format!(
                    "{} is there already: a backup restores into an empty home.",
                    file.display()
                ),
            });
        }
        if let Err(error) = node::write(&file, key, 0o600) {
            undo(&written);
            return Err(error);
        }
        written.push(file);
        let endpoint = overlay::address_of(key.as_slice().try_into().expect("32 bytes"));
        lines.push(format!("connector {connector} onion endpoint: {endpoint}"));
        endpoints.push(json!({ "connector": connector, "onion_endpoint": endpoint }));
    }
    match keystore::create(home, &passphrase, &phrase) {
        Ok(true) => {
            // Where it cannot be kept, the next command that opens the keystore keeps it.
            let _ = event::keep_agent_secret_of(home, &phrase);
        }
        Ok(false) => {
            undo(&written);
            return Err(Error {
                nothing_sent: false,
                unanswered: None,
                code: ErrorCode::Io,
                message: "A wallet was made here while the backup was being restored.".into(),
            });
        }
        Err(error) => {
            undo(&written);
            return Err(error);
        }
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "restored": true, "wallet": wallet, "onion_endpoints": endpoints }),
        text: format!(
            "Wallet restored at {}.\n\n{}\n{}\n\nRun `toon init` to make the TOON app: it keeps these onion endpoints.",
            keystore::path(home).display(),
            listing(&wallet),
            lines.join("\n")
        ),
    })
}

/// `toon wallet`.
pub fn run(home: &Path, command: &WalletCommand) -> Result<Report, Error> {
    match command {
        WalletCommand::Show => show(home),
        WalletCommand::Fund => funding::fund(home),
        WalletCommand::Balances => balances(home),
        WalletCommand::Backup { out } => backup(home, out),
        WalletCommand::Restore { file } => restore(home, file),
    }
}
