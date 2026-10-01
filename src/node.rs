//! The agent node's own state, and the connector config rendered from it.
//!
//! The CLI owns the connector's config. What the operator asked for is recorded in
//! `state.json`; `connector.toml` is rendered from that and from nothing else, every time
//! a connector is about to start, so a hand edit of it does not survive. The keys a
//! connector reads are files the wallet's keys are written to, once, at `init`.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::outcome::{Error, ErrorCode};
use crate::profile::Profile;
use crate::{derive, keystore, overlay};

/// The name of the first TOON app, the one whose connector fronts the relay, and of the
/// relay app behind it.
pub const RELAY: &str = "relay";

/// The connector's route to the relay's paid write endpoint, and its price per write.
pub const RELAY_WRITE_PREFIX: &str = "g.toon.relay";
pub const RELAY_WRITE_PRICE: u64 = 1;
/// The route to the relay's free ephemeral write endpoint.
pub const RELAY_EPHEMERAL_PREFIX: &str = "g.toon.relay.ephemeral";

/// How a TOON app is reached (ADR 0003).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Only at its onion endpoint. The default.
    Hidden,
    /// At a public hostname the operator asked for. The certificate and the reverse
    /// proxy that answer there are the operator's.
    Clearnet { hostname: String },
}

/// What `init` was asked for, for the first TOON app.
#[derive(Clone, Debug)]
pub struct Options {
    /// How it is reached.
    pub reach: Reach,
    /// The operator agreed to the Anyone Protocol's terms, which a hidden service needs.
    pub accept_anyone_terms: bool,
    /// Where the connector listens.
    pub listen: String,
    /// The network profile the chain settings come from.
    pub network: Profile,
    /// The EVM chain it settles on, if any.
    pub evm: Option<Evm>,
    /// The Solana chain it settles on, if the operator opted in.
    pub solana: Option<Solana>,
    /// The faucet `toon wallet fund` asks, if the network has one.
    pub faucet_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evm {
    pub rpc_url: String,
    pub token: String,
    pub decimals: u8,
    pub asset_name: String,
    pub asset_version: String,
    pub transfer_method: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solana {
    pub rpc_url: String,
    pub token: String,
    pub decimals: u8,
}

/// One TOON app as the operator asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToonApp {
    pub name: String,
    /// The index of the wallet's keys this app's connector uses.
    pub connector: u32,
    pub reach: Reach,
    pub listen: String,
    pub evm: Option<Evm>,
    pub solana: Option<Solana>,
    /// The apps behind the connector.
    pub apps: Vec<String>,
}

/// The agent node's state: every TOON app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub network: Profile,
    /// Where `toon wallet fund` asks for funds; the networks without a faucet have none.
    pub faucet_url: Option<String>,
    pub toon_apps: Vec<ToonApp>,
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error {
        code: ErrorCode::Io,
        message: format!("{}: {source}.", path.display()),
    }
}

/// What a command that needs an agent node says when `home` has none.
pub fn no_agent_node(home: &Path) -> Error {
    Error {
        code: ErrorCode::NoAgentNode,
        message: format!("No agent node at {}. Run `toon init`.", home.display()),
    }
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("state.json")
}

/// The wallet's operator write key, written once by `init`. The connector verifies writes
/// with ed25519, and this file holds the 32 bytes the wallet derived, read as an ed25519
/// secret key.
pub fn operator_key(home: &Path) -> PathBuf {
    home.join("operator.key")
}

/// The files of one connector's key material, config, state directory and log.
pub struct ConnectorFiles {
    /// The bearer token the connector's operator surface wants on every read.
    pub bearer_token: PathBuf,
    /// The public keys it accepts a signed write from: the wallet's operator write key.
    pub write_keys: PathBuf,
    pub identity_key: PathBuf,
    pub settlement_key: PathBuf,
    pub solana_settlement_key: PathBuf,
    /// The key a hidden service's onion endpoint is made of.
    pub onion_key: PathBuf,
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub log: PathBuf,
}

impl ConnectorFiles {
    pub fn of(home: &Path, connector: u32) -> Self {
        let dir = home.join("connectors").join(connector.to_string());
        Self {
            bearer_token: dir.join("operator-bearer-token"),
            write_keys: dir.join("operator-write-keys"),
            identity_key: dir.join("identity.key"),
            settlement_key: dir.join("settlement.key"),
            solana_settlement_key: dir.join("settlement-solana.key"),
            onion_key: dir.join("onion.key"),
            config: dir.join("connector.toml"),
            state_dir: dir.join("state"),
            log: dir.join("connector.log"),
        }
    }
}

/// The files of one app behind a connector: the identity key the wallet hands it, and
/// the directory it keeps its data in.
pub struct AppFiles {
    pub identity_key: PathBuf,
    pub data_dir: PathBuf,
}

impl AppFiles {
    pub fn of(home: &Path, app: &str) -> Self {
        let dir = home.join("apps").join(app);
        Self {
            identity_key: dir.join("identity.key"),
            data_dir: dir.join("data"),
        }
    }
}

impl Reach {
    fn json(&self) -> Value {
        match self {
            Reach::Hidden => json!({ "mode": "hidden" }),
            Reach::Clearnet { hostname } => json!({ "mode": "clearnet", "hostname": hostname }),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value["mode"].as_str()? {
            "hidden" => Some(Reach::Hidden),
            "clearnet" => Some(Reach::Clearnet {
                hostname: value["hostname"].as_str()?.to_owned(),
            }),
            _ => None,
        }
    }
}

impl Evm {
    fn json(&self) -> Value {
        json!({
            "rpc_url": self.rpc_url,
            "token": self.token,
            "decimals": self.decimals,
            "asset_name": self.asset_name,
            "asset_version": self.asset_version,
            "transfer_method": self.transfer_method,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        Some(Self {
            rpc_url: text("rpc_url")?,
            token: text("token")?,
            decimals: u8::try_from(value["decimals"].as_u64()?).ok()?,
            asset_name: text("asset_name")?,
            asset_version: text("asset_version")?,
            transfer_method: text("transfer_method")?,
        })
    }
}

impl Solana {
    fn json(&self) -> Value {
        json!({ "rpc_url": self.rpc_url, "token": self.token, "decimals": self.decimals })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            rpc_url: value["rpc_url"].as_str()?.to_owned(),
            token: value["token"].as_str()?.to_owned(),
            decimals: u8::try_from(value["decimals"].as_u64()?).ok()?,
        })
    }
}

impl State {
    /// The state `init` records: one TOON app, the relay's, with the relay behind it.
    pub fn first(options: &Options) -> Self {
        Self {
            network: options.network,
            faucet_url: options.faucet_url.clone(),
            toon_apps: vec![ToonApp {
                name: RELAY.into(),
                connector: 0,
                reach: options.reach.clone(),
                listen: options.listen.clone(),
                evm: options.evm.clone(),
                solana: options.solana.clone(),
                apps: vec![RELAY.into()],
            }],
        }
    }

    fn json(&self) -> Value {
        let apps: Vec<Value> = self
            .toon_apps
            .iter()
            .map(|app| {
                json!({
                    "name": app.name,
                    "connector": app.connector,
                    "reach": app.reach.json(),
                    "listen": app.listen,
                    "evm": app.evm.as_ref().map(Evm::json),
                    "solana": app.solana.as_ref().map(Solana::json),
                    "apps": app.apps,
                })
            })
            .collect();
        json!({
            "version": 1,
            "network": self.network.name(),
            "faucet_url": self.faucet_url,
            "toon_apps": apps,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value["version"] != 1 {
            return None;
        }
        let toon_apps = value["toon_apps"]
            .as_array()?
            .iter()
            .map(|app| {
                Some(ToonApp {
                    name: app["name"].as_str()?.to_owned(),
                    connector: u32::try_from(app["connector"].as_u64()?).ok()?,
                    reach: Reach::from_json(&app["reach"])?,
                    listen: app["listen"].as_str()?.to_owned(),
                    evm: match &app["evm"] {
                        Value::Null => None,
                        evm => Some(Evm::from_json(evm)?),
                    },
                    solana: match &app["solana"] {
                        Value::Null => None,
                        solana => Some(Solana::from_json(solana)?),
                    },
                    apps: app["apps"]
                        .as_array()?
                        .iter()
                        .map(|name| name.as_str().map(str::to_owned))
                        .collect::<Option<_>>()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        // An agent node starts as one TOON app, so a state with none is not one.
        if toon_apps.is_empty() {
            return None;
        }
        let network = match &value["network"] {
            Value::Null => Profile::default(),
            network => Profile::from_name(network.as_str()?)?,
        };
        let faucet_url = match &value["faucet_url"] {
            Value::Null => None,
            url => Some(url.as_str()?.to_owned()),
        };
        Some(Self {
            network,
            faucet_url,
            toon_apps,
        })
    }

    /// The state in `home`, or `None` if there is no agent node there.
    pub fn load(home: &Path) -> Result<Option<Self>, Error> {
        let file = state_path(home);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io(&file, source)),
        };
        serde_json::from_str(&text)
            .ok()
            .and_then(|value| Self::from_json(&value))
            .map(Some)
            .ok_or_else(|| Error {
                code: ErrorCode::Io,
                message: format!("{} is not a state file this version reads.", file.display()),
            })
    }

    /// Record the state, replacing what was there.
    pub fn save(&self, home: &Path) -> Result<(), Error> {
        write(
            &state_path(home),
            format!("{:#}\n", self.json()).as_bytes(),
            0o600,
        )
    }
}

/// Write `bytes` to `path` in full or not at all, readable by this user only.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|source| io(parent, source))?;
    }
    let staged = path.with_extension(format!("{}.tmp", hex::encode(keystore::random::<8>()?)));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staged, path));
    if let Err(source) = written {
        let _ = fs::remove_file(&staged);
        return Err(io(path, source));
    }
    Ok(())
}

/// The line that puts a settlement table's RPC on the overlay's proxy, if there is one.
/// The connector refuses plain http through a circuit, where an exit relay could rewrite the
/// answers, so a plain-http endpoint on this machine, which has nothing to hide from a
/// relay, is the one RPC that is dialed directly.
fn via_proxy(overlay: Option<&Overlay>, rpc_url: &str) -> &'static str {
    let local_http = rpc_url
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .map(|authority| match authority.find(']') {
            Some(end) => &authority[..=end],
            None => authority.split(':').next().unwrap_or(authority),
        })
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]"));
    if overlay.is_some() && !local_http {
        "rpc_via_socks_proxy = true\n"
    } else {
        ""
    }
}

/// A TOML basic string. JSON's escapes are the ones TOML reads.
fn string(text: &str) -> String {
    Value::from(text).to_string()
}

/// The onion endpoint of `app`, if it is a hidden service whose key is there.
pub fn onion_endpoint(home: &Path, app: &ToonApp) -> Option<String> {
    if app.reach != Reach::Hidden {
        return None;
    }
    let key = fs::read(ConnectorFiles::of(home, app.connector).onion_key).ok()?;
    Some(overlay::address_of(key.as_slice().try_into().ok()?))
}

/// What a hidden service's connector is told about the overlay.
pub struct Overlay {
    /// The SOCKS proxy all of its outbound traffic goes through.
    pub proxy: SocketAddr,
    /// Its onion endpoint, a host.
    pub endpoint: String,
}

/// Render the connector config of `app` into `home` and check it with the connector's
/// own validation. `relay` is where the relay's write port is reached, if `app` fronts
/// one. `overlay` is what a hidden service is rendered with: an app that is one has no
/// config without it, and one that is not ignores it. The config is written whether or not it validates, so that the error can be read
/// against it; a caller that gets `Err` starts nothing.
pub fn render(
    home: &Path,
    app: &ToonApp,
    relay: Option<SocketAddr>,
    overlay: Option<&Overlay>,
) -> Result<ConnectorFiles, Error> {
    let overlay = match (&app.reach, overlay) {
        (Reach::Hidden, Some(overlay)) => Some(overlay),
        (Reach::Hidden, None) => {
            return Err(Error {
                code: ErrorCode::OverlayUnavailable,
                message: format!(
                    "{} is a hidden service and the overlay is not there to render it with.",
                    app.name
                ),
            })
        }
        (Reach::Clearnet { .. }, _) => None,
    };
    let files = ConnectorFiles::of(home, app.connector);
    let operator = write_operator_files(home, &files)?;
    // `socks_proxy` is one top-level key, so it goes before the first table: all the
    // connector dials out goes through it, and `socks5h` because no local resolver
    // resolves an onion name.
    let socks = overlay
        .map(|overlay| {
            format!(
                "socks_proxy = {}\n",
                string(&format!("socks5h://{}", overlay.proxy))
            )
        })
        .unwrap_or_default();
    let mut config = format!(
        "# Rendered by `toon` from state.json. Edits here are overwritten.\n\
         client_edge_addr = {}\nstate_dir = {}\n{socks}\n[signer]\nkey_file = {}\n",
        string(&app.listen),
        string(&files.state_dir.to_string_lossy()),
        string(&files.identity_key.to_string_lossy()),
    );
    if let Some(overlay) = overlay {
        config.push_str(&format!(
            "\n[node]\naddresses = [{}]\nhttp_endpoint = {}\nbtp_endpoint = {}\n",
            string(&format!("g.toon.{}", app.name)),
            string(&format!("http://{}/ilp", overlay.endpoint)),
            string(&format!("ws://{}/ilp/btp", overlay.endpoint)),
        ));
    }
    if let Some(relay) = relay.filter(|_| app.apps.iter().any(|name| name == RELAY)) {
        // The relay is paid to write to, and takes a free ephemeral write beside it.
        config.push_str(&format!(
            "\n[[routes]]\nprefix = {}\nhandler_url = {}\nprice = {RELAY_WRITE_PRICE}\n\n\
             [[routes]]\nprefix = {}\nhandler_url = {}\nprice = 0\n",
            string(RELAY_WRITE_PREFIX),
            string(&format!("http://{relay}/write")),
            string(RELAY_EPHEMERAL_PREFIX),
            string(&format!("http://{relay}/write-ephemeral")),
        ));
    }
    if operator {
        config.push_str(&format!(
            "\n[operator]\nbearer_token_file = {}\nwrite_keys_file = {}\n",
            string(&files.bearer_token.to_string_lossy()),
            string(&files.write_keys.to_string_lossy()),
        ));
    }
    if let Some(evm) = &app.evm {
        config.push_str(&format!(
            "\n[settlement.evm]\nrpc_url = {}\ntoken_address = {}\ndecimals = {}\n\
             asset_eip712_name = {}\nasset_eip712_version = {}\nasset_transfer_method = {}\n{}\n\
             [settlement.evm.key]\nkey_file = {}\n",
            string(&evm.rpc_url),
            string(&evm.token),
            evm.decimals,
            string(&evm.asset_name),
            string(&evm.asset_version),
            string(&evm.transfer_method),
            via_proxy(overlay, &evm.rpc_url),
            string(&files.settlement_key.to_string_lossy()),
        ));
    }
    if let Some(solana) = &app.solana {
        config.push_str(&format!(
            "\n[settlement.solana]\nrpc_url = {}\ntoken_address = {}\ndecimals = {}\n\
             min_sponsored_deposit = {}\n{}\n[settlement.solana.key]\nkey_file = {}\n",
            string(&solana.rpc_url),
            string(&solana.token),
            solana.decimals,
            10u64.pow(u32::from(solana.decimals)),
            via_proxy(overlay, &solana.rpc_url),
            string(&files.solana_settlement_key.to_string_lossy()),
        ));
    }
    write(&files.config, config.as_bytes(), 0o600)?;
    fs::create_dir_all(&files.state_dir).map_err(|source| io(&files.state_dir, source))?;
    connector_cli::load_config(&["toon connector", &files.config.to_string_lossy()]).map_err(
        |error| Error {
            code: ErrorCode::ConnectorFailed,
            message: format!("The connector would not accept its config: {error}"),
        },
    )?;
    Ok(files)
}

/// The files the connector's operator surface reads: a bearer token, made once and kept,
/// and the allowlist of write keys, which is the public half of the wallet's operator
/// write key and is written again every time. An agent node made before `init` wrote the
/// operator write key has none, and its connector runs without an operator surface, as it
/// did then: `false`.
fn write_operator_files(home: &Path, files: &ConnectorFiles) -> Result<bool, Error> {
    let key = operator_key(home);
    if !key.exists() {
        return Ok(false);
    }
    if !files.bearer_token.exists() {
        let token = hex::encode(keystore::random::<32>()?);
        write(&files.bearer_token, token.as_bytes(), 0o600)?;
    }
    let bytes = zeroize::Zeroizing::new(fs::read(&key).map_err(|source| io(&key, source))?);
    let secret: zeroize::Zeroizing<[u8; 32]> =
        zeroize::Zeroizing::new(bytes.as_slice().try_into().map_err(|_| Error {
            code: ErrorCode::Io,
            message: format!("{} is not a 32-byte key.", key.display()),
        })?);
    write(
        &files.write_keys,
        format!("{}\n", derive::operator_write_public_key(&secret)).as_bytes(),
        0o600,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_http_rpc_on_this_machine_skips_the_proxy() {
        let overlay = Overlay {
            proxy: "127.0.0.1:9050".parse().unwrap(),
            endpoint: "example.anon".into(),
        };
        for local in [
            "http://localhost:8545",
            "http://127.0.0.1:8545/",
            "http://[::1]:8545",
            "http://[::1]",
        ] {
            assert_eq!(via_proxy(Some(&overlay), local), "", "{local}");
        }
        for remote in [
            "https://localhost:8545",
            "http://rpc.example:8545",
            "http://[::2]:8545",
        ] {
            assert_ne!(via_proxy(Some(&overlay), remote), "", "{remote}");
        }
        assert_eq!(via_proxy(None, "https://rpc.example"), "");
    }
}
