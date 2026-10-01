//! The agent node's own state, and the connector config rendered from it.
//!
//! The CLI owns the connector's config. What the operator asked for is recorded in
//! `state.json`; `connector.toml` is rendered from that and from nothing else, every time
//! a connector is about to start, so a hand edit of it does not survive. The keys a
//! connector reads are files the wallet's keys are written to, once, at `init`.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::keystore;
use crate::outcome::{Error, ErrorCode};

/// The name of the first TOON app, the one whose connector fronts the relay.
pub const RELAY: &str = "relay";

/// What `init` was asked for, for the first TOON app.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where the connector listens.
    pub listen: String,
    /// The EVM chain it settles on, if any.
    pub evm: Option<Evm>,
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

/// One TOON app as the operator asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToonApp {
    pub name: String,
    /// The index of the wallet's keys this app's connector uses.
    pub connector: u32,
    pub listen: String,
    pub evm: Option<Evm>,
    /// The apps behind the connector.
    pub apps: Vec<String>,
}

/// The agent node's state: every TOON app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub toon_apps: Vec<ToonApp>,
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error {
        code: ErrorCode::Io,
        message: format!("{}: {source}.", path.display()),
    }
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("state.json")
}

/// The files of one connector's key material, config, state directory and log.
pub struct ConnectorFiles {
    pub identity_key: PathBuf,
    pub settlement_key: PathBuf,
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub log: PathBuf,
}

impl ConnectorFiles {
    pub fn of(home: &Path, connector: u32) -> Self {
        let dir = home.join("connectors").join(connector.to_string());
        Self {
            identity_key: dir.join("identity.key"),
            settlement_key: dir.join("settlement.key"),
            config: dir.join("connector.toml"),
            state_dir: dir.join("state"),
            log: dir.join("connector.log"),
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

impl State {
    /// The state `init` records: one TOON app, the relay's, with the relay behind it.
    pub fn first(options: &Options) -> Self {
        Self {
            toon_apps: vec![ToonApp {
                name: RELAY.into(),
                connector: 0,
                listen: options.listen.clone(),
                evm: options.evm.clone(),
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
                    "listen": app.listen,
                    "evm": app.evm.as_ref().map(Evm::json),
                    "apps": app.apps,
                })
            })
            .collect();
        json!({ "version": 1, "toon_apps": apps })
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
                    listen: app["listen"].as_str()?.to_owned(),
                    evm: match &app["evm"] {
                        Value::Null => None,
                        evm => Some(Evm::from_json(evm)?),
                    },
                    apps: app["apps"]
                        .as_array()?
                        .iter()
                        .map(|name| name.as_str().map(str::to_owned))
                        .collect::<Option<_>>()?,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self { toon_apps })
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

/// A TOML basic string. JSON's escapes are the ones TOML reads.
fn string(text: &str) -> String {
    Value::from(text).to_string()
}

/// Render the connector config of `app` into `home` and check it with the connector's
/// own validation. The config is written whether or not it validates, so that the
/// error can be read against it; a caller that gets `Err` starts nothing.
pub fn render(home: &Path, app: &ToonApp) -> Result<ConnectorFiles, Error> {
    let files = ConnectorFiles::of(home, app.connector);
    let mut config = format!(
        "# Rendered by `toon` from state.json. Edits here are overwritten.\n\
         client_edge_addr = {}\nstate_dir = {}\n\n[signer]\nkey_file = {}\n",
        string(&app.listen),
        string(&files.state_dir.to_string_lossy()),
        string(&files.identity_key.to_string_lossy()),
    );
    if let Some(evm) = &app.evm {
        config.push_str(&format!(
            "\n[settlement.evm]\nrpc_url = {}\ntoken_address = {}\ndecimals = {}\n\
             asset_eip712_name = {}\nasset_eip712_version = {}\nasset_transfer_method = {}\n\n\
             [settlement.evm.key]\nkey_file = {}\n",
            string(&evm.rpc_url),
            string(&evm.token),
            evm.decimals,
            string(&evm.asset_name),
            string(&evm.asset_version),
            string(&evm.transfer_method),
            string(&files.settlement_key.to_string_lossy()),
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
