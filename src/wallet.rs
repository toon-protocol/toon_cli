//! `toon init` and `toon wallet show`.

use std::path::Path;

use serde_json::{json, Value};

use crate::derive::{self, Addresses};
use crate::keystore;
use crate::node;
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How many connectors a wallet lists. An agent node starts as one TOON app, so one
/// connector; later commands that create TOON apps raise this.
const CONNECTORS: u32 = 1;

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
    let passphrase = keystore::passphrase()?;
    let entropy = zeroize::Zeroizing::new(keystore::random::<16>()?);
    let mnemonic =
        bip39::Mnemonic::from_entropy(&*entropy).expect("16 bytes is a valid entropy length");
    let phrase = zeroize::Zeroizing::new(mnemonic.to_string());
    let wallet = describe(&addresses(&phrase)?);
    // The TOON app is made and checked before the wallet is kept, so that a command line
    // the connector would refuse does not leave a wallet whose mnemonic nobody saw.
    let state = create_toon_app(home, &mnemonic, options)?;
    if !keystore::create(home, &passphrase, &phrase)? {
        return existing(home, options);
    }
    if let Err(error) = state.save(home) {
        // The mnemonic has not been shown, so the wallet goes with the TOON app.
        let _ = std::fs::remove_file(keystore::path(home));
        discard_toon_apps(home);
        return Err(error);
    }
    let text = format!(
        "Wallet created at {}.\n\nYour mnemonic. It is shown this once and no command shows it again; write it down now:\n\n  {}\n\n{}\n\n{}",
        keystore::path(home).display(),
        *phrase,
        listing(&wallet),
        toon_app_text(&state, true)
    );
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "created": true,
            "mnemonic": &*phrase,
            "wallet": wallet,
            "toon_apps": toon_apps(home, &state, true),
        }),
        text,
    })
}

/// Remove what writing a TOON app's keys left.
fn discard_toon_apps(home: &Path) {
    let _ = std::fs::remove_dir_all(home.join("connectors"));
    let _ = std::fs::remove_file(node::operator_key(home));
}

/// Write the keys of the first TOON app's connector, and render and check its config.
/// What it wrote is removed again if it fails.
fn create_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
) -> Result<node::State, Error> {
    let created = write_toon_app(home, mnemonic, options);
    if created.is_err() {
        discard_toon_apps(home);
    }
    created
}

fn write_toon_app(
    home: &Path,
    mnemonic: &bip39::Mnemonic,
    options: &node::Options,
) -> Result<node::State, Error> {
    let seed = derive::seed(mnemonic);
    let state = node::State::first(options);
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
        node::render(home, app)?;
    }
    Ok(state)
}

/// The wallet was there already. If the TOON app is not, make it from the wallet.
fn existing(home: &Path, options: &node::Options) -> Result<Report, Error> {
    let file = keystore::path(home);
    let (state, created) = match node::State::load(home)? {
        Some(state) => (state, false),
        None => {
            let passphrase = keystore::passphrase()?;
            let mnemonic: bip39::Mnemonic =
                keystore::open(home, &passphrase)?
                    .parse()
                    .map_err(|_| Error {
                        code: ErrorCode::KeystoreCorrupt,
                        message: "The keystore does not hold a valid mnemonic.".into(),
                    })?;
            let state = create_toon_app(home, &mnemonic, options)?;
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
        }),
        text: format!(
            "A wallet already exists at {}. {}",
            file.display(),
            toon_app_text(&state, created)
        ),
    })
}

fn toon_apps(home: &Path, state: &node::State, created: bool) -> Vec<Value> {
    state
        .toon_apps
        .iter()
        .map(|app| {
            json!({
                "name": app.name,
                "created": created,
                "config": node::ConnectorFiles::of(home, app.connector).config,
            })
        })
        .collect()
}

fn toon_app_text(state: &node::State, created: bool) -> String {
    let names: Vec<&str> = state
        .toon_apps
        .iter()
        .map(|app| app.name.as_str())
        .collect();
    if created {
        format!(
            "TOON app created: {}. Run `toon up` to start it.",
            names.join(", ")
        )
    } else {
        "Nothing was changed.".into()
    }
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
    let digits = result.trim_start_matches("0x");
    // A 32-byte word whose high half is zero, which no balance outgrows.
    u128::from_str_radix(digits.trim_start_matches('0').max("0"), 16)
        .map_err(|_| chain_failed(format!("'{result}' is not a balance")))
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
