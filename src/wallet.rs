//! `toon init` and `toon wallet show`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use serde_json::{json, Value};

use crate::derive::{self, Addresses};
use crate::funding;
use crate::keystore;
use crate::node::{self, Reach};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::overlay::{self, Edge};
use crate::runner;

/// How many connectors a wallet lists. An agent node starts as one TOON app, so one
/// connector; later commands that create TOON apps raise this.
const CONNECTORS: u32 = 1;

/// Which relay's identity key the first TOON app's relay gets.
const RELAY_INDEX: u32 = 0;

fn addresses(mnemonic: &str) -> Result<Addresses, Error> {
    let mnemonic: bip39::Mnemonic = mnemonic.parse().map_err(|_| Error {
        code: ErrorCode::KeystoreCorrupt,
        message: "The keystore does not hold a valid mnemonic.".into(),
    })?;
    derive::addresses(&*derive::seed(&mnemonic), CONNECTORS).map_err(|source| Error {
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
pub fn init(home: &Path, options: &node::Options) -> Result<Report, Error> {
    if keystore::exists(home) {
        return existing(home, options);
    }
    if node::State::load(home)?.is_some() {
        return Err(Error {
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
    let entropy = zeroize::Zeroizing::new(keystore::random::<16>()?);
    let mnemonic =
        bip39::Mnemonic::from_entropy(&*entropy).expect("16 bytes is a valid entropy length");
    let phrase = zeroize::Zeroizing::new(mnemonic.to_string());
    let wallet = describe(&addresses(&phrase)?);
    // The TOON app is made and checked before the wallet is kept, so that a command line
    // the connector would refuse does not leave a wallet whose mnemonic nobody saw.
    let state = create_toon_app(home, &mnemonic, options, edge.as_deref())?;
    if !keystore::create(home, &passphrase, &phrase)? {
        return existing(home, options);
    }
    if let Err(error) = state.save(home) {
        // The mnemonic has not been shown, so the wallet goes with the TOON app.
        let _ = std::fs::remove_file(keystore::path(home));
        discard_toon_apps(home);
        return Err(error);
    }
    let (needs, funding_text) = funding::requirements(home, &state)?;
    let text = format!(
        "Wallet created at {}.\n\nYour mnemonic. It is shown this once and no command shows it again; write it down now:\n\n  {}\n\n{}\n\n{}\n\n{}",
        keystore::path(home).display(),
        *phrase,
        listing(&wallet),
        toon_app_text(home, &state, true),
        funding_text
    );
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "created": true,
            "mnemonic": &*phrase,
            "wallet": wallet,
            "toon_apps": toon_apps(home, &state, true),
            "notes": notes(&state),
            "network": state.network.name(),
            "needs": needs.iter().map(funding::Need::json).collect::<Vec<_>>(),
        }),
        text,
    })
}

/// Remove what writing a TOON app's keys left.
fn discard_toon_apps(home: &Path) {
    let _ = std::fs::remove_dir_all(home.join("connectors"));
    let _ = std::fs::remove_dir_all(home.join("apps"));
    let _ = std::fs::remove_dir_all(home.join("overlay"));
    let _ = std::fs::remove_file(node::operator_key(home));
}

/// Write the keys of the first TOON app's connector, and render and check its config.
/// What it wrote is removed again if it fails.
fn create_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
    edge: Option<&dyn Edge>,
) -> Result<node::State, Error> {
    let created = write_toon_app(home, mnemonic, options, edge);
    if created.is_err() {
        discard_toon_apps(home);
    }
    created
}

fn write_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
    edge: Option<&dyn Edge>,
) -> Result<node::State, Error> {
    let seed = derive::seed(mnemonic);
    // The port is chosen now, once, so that the address a connector publishes to its
    // peers is the same one every time it starts.
    let options = node::Options {
        listen: node::concrete(&options.listen)?,
        ..options.clone()
    };
    let state = node::State::first(&options);
    let operator = derive::operator_write_secret(&*seed).map_err(|source| Error {
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })?;
    node::write(&node::operator_key(home), &*operator, 0o600)?;
    for app in &state.toon_apps {
        let files = node::ConnectorFiles::of(home, app.connector);
        let corrupt = |source: derive::DeriveError| Error {
            code: ErrorCode::KeystoreCorrupt,
            message: source.0,
        };
        let identity = derive::identity_secret(&*seed, app.connector).map_err(corrupt)?;
        let settlement = derive::evm_settlement_secret(&*seed, app.connector).map_err(corrupt)?;
        node::write(&files.identity_key, &*identity, 0o600)?;
        node::write(&files.settlement_key, &*settlement, 0o600)?;
        if app.solana.is_some() {
            let solana =
                derive::solana_settlement_secret(&*seed, app.connector).map_err(corrupt)?;
            node::write(&files.solana_settlement_key, &*solana, 0o600)?;
        }
        if app.apps.iter().any(|name| name == node::RELAY) {
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
                let onion = derive::onion_secret(&*seed, app.connector).map_err(corrupt)?;
                node::write(&files.onion_key, &*onion, 0o600)?;
                Some(node::Overlay {
                    proxy: edge.proxy(),
                    endpoint: edge.issue(app.connector, &files.onion_key)?,
                })
            }
            _ => None,
        };
        node::render(home, app, Some(placeholder), overlay.as_ref())?;
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
                        code: ErrorCode::KeystoreCorrupt,
                        message: "The keystore does not hold a valid mnemonic.".into(),
                    })?;
            let state = create_toon_app(home, &mnemonic, options, edge.as_deref())?;
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
                            .any(|name| name == node::RELAY)
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
                if app.apps.iter().any(|name| name == node::RELAY) {
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

fn notes(state: &node::State) -> Vec<&'static str> {
    if state.toon_apps.iter().any(|app| app.reach == Reach::Hidden) {
        vec![HIDDEN_NOTE]
    } else {
        Vec::new()
    }
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
    if let Reach::Clearnet { hostname } = &options.reach {
        if hostname.is_empty()
            || hostname.contains(|c: char| c.is_whitespace() || "/:@".contains(c))
        {
            return Err(Error {
                code: ErrorCode::Usage,
                message: format!("`--clearnet` takes a hostname, and {hostname:?} is not one."),
            });
        }
        return Ok(None);
    }
    if !options.accept_anyone_terms {
        return Err(Error {
            code: ErrorCode::Usage,
            message: "A new TOON app is a hidden service on the Anyone overlay. Pass \
                      `--accept-anyone-terms` to agree to the Anyone Protocol's terms, or \
                      `--clearnet <hostname>` to ask for clearnet instead."
                .into(),
        });
    }
    if options
        .listen
        .parse::<SocketAddr>()
        .is_ok_and(|listen| !listen.ip().is_loopback())
    {
        return Err(Error {
            code: ErrorCode::Usage,
            message: format!(
                "A hidden service listens on loopback only, and {} is not: its onion endpoint \
                 is the only address it is reached at. `--clearnet <hostname>` binds an address \
                 for a hostname instead.",
                options.listen
            ),
        });
    }
    overlay::bootstrap(home).map(Some)
}

/// List the wallet's addresses.
pub fn show(home: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let passphrase = keystore::passphrase()?;
    let mnemonic = keystore::open(home, &passphrase)?;
    let wallet = describe(&addresses(&mnemonic)?);
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

/// One JSON-RPC call to `rpc_url`, whose `result` is a hex quantity.
fn quantity(rpc_url: &str, method: &str, params: Value) -> Result<u128, Error> {
    let chain_failed = |message: String| Error {
        code: ErrorCode::ChainFailed,
        message: format!("{method} to {rpc_url}: {message}."),
    };
    let reply: Value = reqwest::blocking::Client::builder()
        .timeout(CHAIN_PATIENCE)
        .build()
        .and_then(|client| {
            client
                .post(rpc_url)
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
                .send()
        })
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| chain_failed(error.to_string()))?;
    if let Some(error) = reply.get("error") {
        return Err(chain_failed(format!("the chain refused: {error}")));
    }
    let result = reply["result"]
        .as_str()
        .ok_or_else(|| chain_failed("the answer has no result".into()))?;
    // A 32-byte word. A balance that does not fit its low 16 bytes is refused, not cut.
    let digits = result.trim_start_matches("0x").trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(digits, 16)
        .map_err(|_| chain_failed(format!("'{result}' is not a balance this can show")))
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
    let addresses = addresses(&mnemonic)?;
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    for app in &state.toon_apps {
        let keys = addresses
            .connectors
            .iter()
            .find(|keys| keys.index == app.connector)
            .ok_or_else(|| Error {
                code: ErrorCode::KeystoreCorrupt,
                message: format!("The wallet has no keys for connector {}.", app.connector),
            })?;
        let evm = match &app.evm {
            Some(evm) => {
                let native = quantity(&evm.rpc_url, "eth_getBalance", json!([keys.evm, "latest"]))?;
                let token = quantity(
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
        lines.push(format!(
            "{} solana {}: no chain configured",
            app.name, keys.solana
        ));
        entries.push(json!({
            "toon_app": app.name,
            "chain": "solana",
            "address": keys.solana,
            "native": null,
            "token": null,
        }));
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "balances": entries }),
        text: lines.join("\n"),
    })
}
