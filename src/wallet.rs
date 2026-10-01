//! `toon init` and `toon wallet show`.

use std::path::Path;

use serde_json::{json, Value};

use crate::derive::{self, Addresses};
use crate::keystore;
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

/// Create the wallet, once. A second `init` finds the first and changes nothing.
pub fn init(home: &Path) -> Result<Report, Error> {
    if keystore::exists(home) {
        return Ok(existing(home));
    }
    let passphrase = keystore::passphrase()?;
    let mut entropy = [0u8; 16];
    getrandom::getrandom(&mut entropy).map_err(|source| Error {
        code: ErrorCode::Io,
        message: format!("No source of randomness: {source}."),
    })?;
    let mnemonic =
        bip39::Mnemonic::from_entropy(&entropy).expect("16 bytes is a valid entropy length");
    let phrase = zeroize::Zeroizing::new(mnemonic.to_string());
    let wallet = describe(&addresses(&phrase)?);
    if !keystore::create(home, &passphrase, &phrase)? {
        return Ok(existing(home));
    }
    let text = format!(
        "Wallet created at {}.\n\nYour mnemonic. It is shown this once and no command shows it again; write it down now:\n\n  {}\n\n{}",
        keystore::path(home).display(),
        *phrase,
        listing(&wallet)
    );
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "created": true, "mnemonic": &*phrase, "wallet": wallet }),
        text,
    })
}

fn existing(home: &Path) -> Report {
    let file = keystore::path(home);
    Report {
        exit: Exit::Success,
        json: json!({ "created": false, "keystore": file }),
        text: format!(
            "A wallet already exists at {}. Nothing was changed.",
            file.display()
        ),
    }
}

/// List the wallet's addresses.
pub fn show(home: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(Error {
            code: ErrorCode::NoWallet,
            message: format!("There is no wallet at {}. Run `toon init`.", home.display()),
        });
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
