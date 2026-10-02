//! What an agent node's settlement keys must hold before a connector starts, and
//! `toon wallet fund`, which asks the devnet faucet for it.
//!
//! The keys are read from the files `init` wrote, so neither needs the passphrase.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::derive;
use crate::node::{self, ConnectorFiles, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::profile::Profile;

/// Gas an EVM settlement key must hold, in wei: 0.0001 ETH.
const EVM_GAS: u128 = 100_000_000_000_000;
/// What a Solana settlement key must hold for fees and rent, in lamports: 0.01 SOL.
const SOLANA_GAS: u128 = 10_000_000;
/// What the faucet is asked for as SOL, in lamports: 1 SOL.
const SOLANA_AIRDROP: u64 = 1_000_000_000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// What a need is for: a connector to start, or a transaction its key sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Without it the connector does not start.
    Start,
    /// A deposit, or another transaction from the key, spends it.
    Deposit,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::Start => "start",
            Purpose::Deposit => "deposit",
        }
    }
}

/// One thing an address must hold: gas, or the token the connector is paid in.
#[derive(Clone, Debug)]
pub struct Need {
    pub chain: &'static str,
    pub address: String,
    /// `ETH` or `SOL` for gas, else the token's address.
    pub asset: String,
    /// In the asset's base units.
    pub amount: u128,
    /// How the amount reads to a person.
    pub shown: String,
    gas: bool,
    /// What it is held for.
    pub purpose: Purpose,
    /// Where to read the balance, and which token.
    rpc_url: String,
    token: String,
}

impl Need {
    pub fn text(&self) -> String {
        format!("{} ({}): {}", self.address, self.chain, self.shown)
    }

    pub fn json(&self) -> Value {
        json!({
            "chain": self.chain,
            "address": self.address,
            "asset": self.asset,
            "amount": self.amount.to_string(),
            "shown": self.shown,
            "for": self.purpose.as_str(),
        })
    }
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error {
        nothing_sent: false,
        code: ErrorCode::Io,
        message: format!("{}: {source}.", path.display()),
    }
}

fn secret(path: &Path) -> Result<[u8; 32], Error> {
    let bytes = fs::read(path).map_err(|source| io(path, source))?;
    bytes.try_into().map_err(|_| Error {
        nothing_sent: false,
        code: ErrorCode::KeystoreCorrupt,
        message: format!("{} is not a 32-byte key.", path.display()),
    })
}

/// What the settlement keys of `app` must hold, for every chain it settles on. An EVM
/// connector starts without gas: its boot only reads the chain, and gas is spent when it
/// sends a transaction. A Solana connector sends one at boot, so its gas is for starting.
pub fn needs(home: &Path, app: &ToonApp) -> Result<Vec<Need>, Error> {
    let files = ConnectorFiles::of(home, app.connector);
    let mut needs = Vec::new();
    if let Some(evm) = &app.evm {
        let address = derive::evm_address(&secret(&files.settlement_key)?);
        let token = 10u128.pow(u32::from(evm.decimals));
        for (gas, purpose, asset, amount, shown) in [
            (
                true,
                Purpose::Deposit,
                "ETH".to_owned(),
                EVM_GAS,
                "0.0001 ETH for gas".to_owned(),
            ),
            (
                false,
                Purpose::Start,
                evm.token.clone(),
                token,
                format!("1 token ({token} base units of {})", evm.token),
            ),
        ] {
            needs.push(Need {
                chain: "evm",
                address: address.clone(),
                asset,
                amount,
                shown,
                gas,
                purpose,
                rpc_url: evm.rpc_url.clone(),
                token: evm.token.clone(),
            });
        }
    }
    if let Some(solana) = &app.solana {
        let address = derive::solana_address(&secret(&files.solana_settlement_key)?);
        let token = 10u128.pow(u32::from(solana.decimals));
        for (gas, purpose, asset, amount, shown) in [
            (
                true,
                Purpose::Start,
                "SOL".to_owned(),
                SOLANA_GAS,
                "0.01 SOL for fees".to_owned(),
            ),
            (
                false,
                Purpose::Start,
                solana.token.clone(),
                token,
                format!("1 token ({token} base units of {})", solana.token),
            ),
        ] {
            needs.push(Need {
                chain: "solana",
                address: address.clone(),
                asset,
                amount,
                shown,
                gas,
                purpose,
                rpc_url: solana.rpc_url.clone(),
                token: solana.token.clone(),
            });
        }
    }
    Ok(needs)
}

/// What a connector needs to start.
pub fn start_needs(home: &Path, app: &ToonApp) -> Result<Vec<Need>, Error> {
    let mut needs = needs(home, app)?;
    needs.retain(|need| need.purpose == Purpose::Start);
    Ok(needs)
}

/// What a transaction from the settlement key of `app` needs.
fn deposit_needs(home: &Path, app: &ToonApp) -> Result<Vec<Need>, Error> {
    let mut needs = needs(home, app)?;
    needs.retain(|need| need.purpose == Purpose::Deposit);
    Ok(needs)
}

/// Refuse with `unfunded` unless the settlement key of `app` holds the gas a deposit
/// spends. A chain that cannot be asked is not a verdict: the connector answers for itself.
pub fn ensure_gas(home: &Path, network: Profile, app: &ToonApp) -> Result<(), Error> {
    let lacking = shortfalls(deposit_needs(home, app)?).unwrap_or_default();
    if lacking.is_empty() {
        return Ok(());
    }
    Err(unfunded(
        network,
        &format!(
            "The settlement key of {} has no gas for a transaction, so nothing was sent.",
            app.name
        ),
        &lacking,
    ))
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .expect("an HTTP client with a timeout")
}

fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let reply: Value = client()
        .post(url)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .and_then(|response| response.json())
        .map_err(|error| format!("{method} to {url}: {error}"))?;
    match reply.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(format!("{method} to {url}: {}", reply["error"])),
    }
}

/// A hex quantity or word, saturating at `u128::MAX`.
fn hex_amount(text: &str) -> Option<u128> {
    let digits = text.trim_start_matches("0x").trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if digits.len() > 32 {
        return Some(u128::MAX);
    }
    u128::from_str_radix(digits, 16).ok()
}

fn balance(need: &Need) -> Result<u128, String> {
    let bad = |what: &str| format!("{}: {what}", need.rpc_url);
    match (need.chain, need.gas) {
        ("evm", true) => {
            let result = rpc(
                &need.rpc_url,
                "eth_getBalance",
                json!([need.address, "latest"]),
            )?;
            hex_amount(result.as_str().unwrap_or_default()).ok_or_else(|| bad("no balance"))
        }
        ("evm", false) => {
            let owner = need.address.trim_start_matches("0x").to_lowercase();
            let data = format!("0x70a08231{owner:0>64}");
            let result = rpc(
                &need.rpc_url,
                "eth_call",
                json!([{ "to": need.token, "data": data }, "latest"]),
            )?;
            hex_amount(result.as_str().unwrap_or_default()).ok_or_else(|| bad("no balance"))
        }
        (_, true) => {
            let result = rpc(&need.rpc_url, "getBalance", json!([need.address]))?;
            result["value"]
                .as_u64()
                .map(u128::from)
                .ok_or_else(|| bad("no balance"))
        }
        (_, false) => {
            let result = rpc(
                &need.rpc_url,
                "getTokenAccountsByOwner",
                json!([need.address, { "mint": need.token }, { "encoding": "jsonParsed" }]),
            )?;
            Ok(result["value"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|account| {
                    account["account"]["data"]["parsed"]["info"]["tokenAmount"]["amount"]
                        .as_str()?
                        .parse::<u128>()
                        .ok()
                })
                .sum())
        }
    }
}

/// Which of `needs` the chains say is not yet held, or why a balance could not be read.
pub fn shortfalls(needs: Vec<Need>) -> Result<Vec<Need>, String> {
    let mut lacking = Vec::new();
    for need in needs {
        if balance(&need)? < need.amount {
            lacking.push(need);
        }
    }
    Ok(lacking)
}

/// How to get what is lacking, for the network the agent node is on.
fn how_to_fund(network: Profile, lacking: &[Need]) -> String {
    let token = lacking.iter().any(|need| !need.gas);
    let eth = lacking.iter().any(|need| need.gas && need.asset == "ETH");
    match network {
        Profile::Devnet => {
            let mut advice = Vec::new();
            if token || lacking.iter().any(|need| need.gas && need.asset != "ETH") {
                advice.push("Run `toon wallet fund` to fund them from the devnet faucet.".to_owned());
            }
            if eth {
                advice.push(
                    "The devnet faucet sends no ETH: Base Sepolia ETH comes from a public Base Sepolia faucet, which you give the EVM address named here."
                        .to_owned(),
                );
            }
            advice.join(" ")
        }
        Profile::Sandbox => "Fund them from the sandbox's own chain tooling; `toon wallet fund` has no faucet there.".to_owned(),
        Profile::Mainnet => "This is mainnet: fund them yourself, with your own money. `toon` has no way to fund them. Run the command again once they are funded.".to_owned(),
    }
}

/// The refusal for a settlement key that is not funded. `what` says what was not done.
pub fn unfunded(network: Profile, what: &str, lacking: &[Need]) -> Error {
    let list: Vec<String> = lacking.iter().map(Need::text).collect();
    Error {
        nothing_sent: false,
        code: ErrorCode::Unfunded,
        message: format!(
            "{what} It needs: {}. {}",
            list.join("; "),
            how_to_fund(network, lacking)
        ),
    }
}

/// The refusal of `toon up`: the connector is not started.
pub fn unfunded_to_start(network: Profile, lacking: &[Need]) -> Error {
    unfunded(
        network,
        "A settlement key is not funded, so the connector is not started.",
        lacking,
    )
}

/// What `init` tells the operator to fund, and how.
pub fn requirements(home: &Path, state: &State) -> Result<(Vec<Need>, String), Error> {
    let mut all = Vec::new();
    for app in &state.toon_apps {
        all.extend(needs(home, app)?);
    }
    let text = if all.is_empty() {
        String::new()
    } else {
        let part = |purpose: Purpose| -> Vec<Need> {
            all.iter()
                .filter(|need| need.purpose == purpose)
                .cloned()
                .collect()
        };
        let lines = |needs: &[Need]| -> String {
            needs
                .iter()
                .map(|need| format!("  {}", need.text()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (start, deposit) = (part(Purpose::Start), part(Purpose::Deposit));
        let mut text = format!(
            "Before `toon up` can start the connector, fund:\n{}\n{}",
            lines(&start),
            how_to_fund(state.network, &start)
        );
        if !deposit.is_empty() {
            text.push_str(&format!(
                "\nA deposit (`toon join`, `toon peer add`, `toon create`, `toon channel`) spends gas, which `toon up` does not need:\n{}\n{}",
                lines(&deposit),
                how_to_fund(state.network, &deposit)
            ));
        }
        text
    };
    Ok((all, text))
}

fn faucet_error(message: String) -> Error {
    Error {
        nothing_sent: false,
        code: ErrorCode::FaucetUnavailable,
        message,
    }
}

fn ask(faucet: &str, path: &str, address: &str) -> Result<Value, Error> {
    let url = format!("{}{path}", faucet.trim_end_matches('/'));
    let response = client()
        .post(&url)
        .json(&json!({ "address": address }))
        .send()
        .map_err(|error| faucet_error(format!("The faucet at {url} did not answer: {error}.")))?;
    let status = response.status();
    let body: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let why = body["message"]
            .as_str()
            .or_else(|| body["error"].as_str())
            .unwrap_or("no reason given");
        return Err(faucet_error(format!(
            "The faucet at {url} refused {address} ({status}): {why}."
        )));
    }
    Ok(body)
}

/// `toon wallet fund`: ask the devnet faucet for every settlement address, then say
/// what is still lacking.
pub fn fund(home: &Path) -> Result<Report, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let faucet = match (state.network, &state.faucet_url) {
        (Profile::Devnet, Some(faucet)) => faucet,
        (Profile::Mainnet, _) => {
            return Err(faucet_error(
                "This is mainnet: there is no faucet. Fund the addresses yourself, with your own money; `toon wallet show` lists them."
                    .into(),
            ))
        }
        (network, _) => {
            return Err(faucet_error(format!(
                "The {} network has no faucet to ask. Fund the addresses from that network's own tooling; `toon wallet show` lists them.",
                network.name()
            )))
        }
    };
    let mut funded = Vec::new();
    for app in &state.toon_apps {
        let files = ConnectorFiles::of(home, app.connector);
        if app.evm.is_some() {
            let address = derive::evm_address(&secret(&files.settlement_key)?);
            let reply = ask(faucet, "/api/base-sepolia/request", &address)?;
            funded.push(json!({ "chain": "evm", "address": address, "faucet": reply }));
        }
        if let Some(solana) = &app.solana {
            let address = derive::solana_address(&secret(&files.solana_settlement_key)?);
            let reply = ask(faucet, "/api/solana/usdc-request", &address)?;
            // The faucet mints the token; the chain's own airdrop pays for fees.
            let airdrop = rpc(
                &solana.rpc_url,
                "requestAirdrop",
                json!([address, SOLANA_AIRDROP]),
            )
            .map(|signature| json!({ "signature": signature }))
            .unwrap_or_else(|error| json!({ "error": error }));
            funded.push(json!({
                "chain": "solana", "address": address, "faucet": reply, "airdrop": airdrop,
            }));
        }
    }
    let mut all = Vec::new();
    for app in &state.toon_apps {
        all.extend(needs(home, app)?);
    }
    // The faucet has been asked either way: a balance that cannot be read is said, not
    // a failure of the command.
    let lacking = shortfalls(all);
    let mut text = String::from("Asked the devnet faucet for:\n");
    for entry in &funded {
        text.push_str(&format!(
            "  {} ({})\n",
            entry["address"].as_str().unwrap_or_default(),
            entry["chain"].as_str().unwrap_or_default()
        ));
    }
    match &lacking {
        Ok(lacking) if lacking.is_empty() => {
            text.push_str("Every settlement key is funded. Run `toon up`.");
        }
        Ok(lacking) => {
            let (start, deposit): (Vec<Need>, Vec<Need>) = lacking
                .iter()
                .cloned()
                .partition(|need| need.purpose == Purpose::Start);
            let list = |needs: &[Need]| -> String {
                needs.iter().map(Need::text).collect::<Vec<_>>().join("; ")
            };
            if start.is_empty() {
                text.push_str("Every settlement key holds what `toon up` needs. Run `toon up`.");
            } else {
                text.push_str(&format!(
                    "Still lacking, which the faucet did not send: {}.",
                    list(&start)
                ));
            }
            if !deposit.is_empty() {
                text.push_str(&format!(
                    "\nThe gas a deposit needs is not held: {}. {}",
                    list(&deposit),
                    how_to_fund(state.network, &deposit)
                ));
            }
        }
        Err(message) => {
            text.push_str(&format!(
                "The settlement keys' balances could not be read: {message}."
            ));
        }
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "funded": funded,
            "lacking": lacking
                .as_ref()
                .map(|lacking| lacking.iter().map(Need::json).collect::<Vec<_>>())
                .ok(),
        }),
        text,
    })
}
