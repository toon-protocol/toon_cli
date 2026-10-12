//! Agent keys (ADR 0010): operator write keys with a scope and a spending limit, which the
//! operator hands to an agent that does not hold the wallet.
//!
//! The home holds the record of them, `agent-keys.json`. `toon` writes each connector's
//! `controller_keys_file` from it, on every change, and the connector reads that file again
//! on every signed request, so a change reaches a running connector at once and a revoked key
//! is refused on its next write. A key is on the list of the TOON apps it is granted on and
//! of no other.
//!
//! The record never holds a private key. `create` writes that once, to a key file the owner
//! alone can read, and keeps nothing of it.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{AgentKeyCommand, AgentKeyCreateArgs, AgentKeySetArgs};
use crate::node::{self, ConnectorFiles, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::{keystore, ports, spending};

/// The scopes of the supervisor's own orders. The supervisor answers them; the connector
/// knows none of them.
const ORDER_SCOPES: [&str; 6] = [
    "status",
    "logs",
    "apps",
    "route-price",
    "sign",
    "read-messages",
];

/// The scopes that name writes of a connector. `send`, `peer` and `routes` are `toon`'s words
/// for the connector's `packets`, `peers` and the two route writes; a channel write is named
/// as the connector names it.
const WRITE_SCOPES: [&str; 7] = [
    "send",
    "peer",
    "routes",
    "channels-open",
    "channels-fund",
    "channels-withdraw",
    "channels-land",
];

/// What a key may do when `--scope` is not given: every order, and the writes for send, peer
/// and routes. Nothing that withdraws from or closes a channel.
const DEFAULT_WRITES: [&str; 3] = ["send", "peer", "routes"];

/// How many times a day a key may restart an app, when `--max-restarts` is not given.
const DEFAULT_RESTARTS: u32 = 10;

const RECORD: &str = "agent-keys.json";

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
        unanswered: None,
    }
}

fn usage(message: String) -> Error {
    failed(ErrorCode::Usage, message)
}

/// One agent key as the home records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// The public half, 64 hex characters.
    pub public: String,
    pub scope: Vec<String>,
    pub max_per_command: u64,
    pub max_per_day: u64,
    pub max_restarts: u32,
    /// RFC 3339.
    pub expires: Option<String>,
    /// The TOON apps it is granted on.
    pub toon_apps: Vec<String>,
}

impl Key {
    fn json(&self) -> Value {
        json!({
            "public_key": self.public,
            "scope": self.scope,
            "spending_limit": {
                "per_command": self.max_per_command,
                "per_day": self.max_per_day,
            },
            "max_restarts": self.max_restarts,
            "expires": self.expires,
            "toon_apps": self.toon_apps,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let strings = |field: &str| -> Option<Vec<String>> {
            value[field]
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect()
        };
        Some(Self {
            public: value["public_key"].as_str()?.to_owned(),
            scope: strings("scope")?,
            max_per_command: value["spending_limit"]["per_command"].as_u64()?,
            max_per_day: value["spending_limit"]["per_day"].as_u64()?,
            max_restarts: u32::try_from(value["max_restarts"].as_u64()?).ok()?,
            expires: value["expires"].as_str().map(str::to_owned),
            toon_apps: strings("toon_apps")?,
        })
    }

    /// The connector's writes the scope names, sorted and without repeats.
    fn connector_scope(&self) -> Vec<&'static str> {
        let mut writes: Vec<&'static str> = Vec::new();
        for name in &self.scope {
            match name.as_str() {
                "send" => writes.push("packets"),
                "peer" => writes.push("peers"),
                "routes" => writes.extend(["routes-peers", "routes-leased"]),
                "channels-open" => writes.push("channels-open"),
                "channels-fund" => writes.push("channels-fund"),
                "channels-withdraw" => writes.push("channels-withdraw"),
                "channels-land" => writes.push("channels-land"),
                _ => {}
            }
        }
        writes.sort_unstable();
        writes.dedup();
        writes
    }
}

/// The record of agent keys.
#[derive(Debug, Default)]
pub struct Record {
    /// Where the supervisor listens for orders, once a key has been made.
    pub supervisor: Option<String>,
    pub keys: Vec<Key>,
}

fn record_path(home: &Path) -> PathBuf {
    home.join(RECORD)
}

impl Record {
    pub fn load(home: &Path) -> Result<Self, Error> {
        let path = record_path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(source) => {
                return Err(failed(
                    ErrorCode::Io,
                    format!("{}: {source}.", path.display()),
                ))
            }
        };
        let corrupt = || {
            failed(
                ErrorCode::Io,
                format!(
                    "{} is not a record of agent keys this version reads.",
                    path.display()
                ),
            )
        };
        let value: Value = serde_json::from_str(&text).map_err(|_| corrupt())?;
        let keys = value["keys"]
            .as_array()
            .ok_or_else(corrupt)?
            .iter()
            .map(|key| Key::from_json(key).ok_or_else(corrupt))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            supervisor: value["supervisor"].as_str().map(str::to_owned),
            keys,
        })
    }

    fn save(&self, home: &Path) -> Result<(), Error> {
        let value = json!({
            "supervisor": self.supervisor,
            "keys": self.keys.iter().map(Key::json).collect::<Vec<_>>(),
        });
        node::write(&record_path(home), format!("{value}\n").as_bytes(), 0o600)
    }

    fn find(&self, public: &str) -> Result<usize, Error> {
        self.keys
            .iter()
            .position(|key| key.public == public)
            .ok_or_else(|| {
                failed(
                    ErrorCode::UnknownName,
                    format!("No agent key {public}. `toon agent-key list` shows the keys."),
                )
            })
    }
}

/// The name of a key's table in a connector's list: the whole public half, since two
/// tables of one name make the connector refuse the whole list.
fn table_name(public: &str) -> String {
    format!("agent-{public}")
}

/// The connector's `controller_keys_file` for the TOON app `toon_app`: the keys granted on it
/// that make a write the connector knows. A key whose scope holds only orders is on no list.
fn connector_list(record: &Record, toon_app: &str) -> String {
    let mut list = String::new();
    for key in record
        .keys
        .iter()
        .filter(|key| key.toon_apps.iter().any(|app| app == toon_app))
    {
        let writes = key.connector_scope();
        if writes.is_empty() {
            continue;
        }
        let names: Vec<String> = writes.iter().map(|write| format!("\"{write}\"")).collect();
        list.push_str(&format!(
            "[{}]\nkey = \"{}\"\nscope = [{}]\n",
            table_name(&key.public),
            key.public,
            names.join(", ")
        ));
        if let Some(expires) = &key.expires {
            list.push_str(&format!("expires = \"{expires}\"\n"));
        }
        let limit = format!(
            "{{ per_write = {}, per_day = {} }}",
            key.max_per_command, key.max_per_day
        );
        if writes.contains(&"packets") {
            list.push_str(&format!("spending_limit = {limit}\n"));
        }
        if ["peers", "channels-open", "channels-fund"]
            .iter()
            .any(|write| writes.contains(write))
        {
            list.push_str(&format!("deposit_limit = {limit}\n"));
        }
        list.push('\n');
    }
    list
}

/// Write the list of agent keys granted on `toon_app` to where its connector reads it.
pub fn write_connector_list(
    home: &Path,
    files: &ConnectorFiles,
    toon_app: &str,
) -> Result<(), Error> {
    let record = Record::load(home)?;
    node::write(
        &files.controller_keys,
        connector_list(&record, toon_app).as_bytes(),
        0o600,
    )
}

/// Write every connector's list from the record: what a change to a key does.
fn write_lists(home: &Path, state: &State) -> Result<(), Error> {
    for app in &state.toon_apps {
        write_connector_list(home, &ConnectorFiles::of(home, app.connector), &app.name)?;
    }
    Ok(())
}

fn state(home: &Path) -> Result<State, Error> {
    State::load(home)?.ok_or_else(|| node::no_agent_node(home))
}

/// Open the keystore, to prove the passphrase, and keep the agent identity's secret for the
/// supervisor when `scope` lets the key sign or read messages: the supervisor has no
/// passphrase to derive it with (ADR 0008).
fn require_passphrase(home: &Path, scope: &[String]) -> Result<(), Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let passphrase = keystore::passphrase()?;
    let phrase = keystore::open(home, &passphrase)?;
    if scope
        .iter()
        .any(|name| name == "sign" || name == "read-messages")
    {
        crate::event::keep_agent_secret_of(home, &phrase)?;
    }
    Ok(())
}

/// The scope as given, checked: the default if none was.
fn parse_scope(given: &[String]) -> Result<Vec<String>, Error> {
    if given.is_empty() {
        return Ok(ORDER_SCOPES
            .iter()
            .chain(DEFAULT_WRITES.iter())
            .map(|name| (*name).to_owned())
            .collect());
    }
    let mut scope: Vec<String> = Vec::new();
    for name in given {
        if !ORDER_SCOPES.contains(&name.as_str()) && !WRITE_SCOPES.contains(&name.as_str()) {
            return Err(usage(format!(
                "'{name}' is not a scope. The scopes are {}.",
                ORDER_SCOPES
                    .iter()
                    .chain(WRITE_SCOPES.iter())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if !scope.contains(name) {
            scope.push(name.clone());
        }
    }
    Ok(scope)
}

fn check_expiry(expires: &str) -> Result<(), Error> {
    chrono::DateTime::parse_from_rfc3339(expires)
        .map(|_| ())
        .map_err(|error| {
            usage(format!(
                "--expires '{expires}' is not an RFC 3339 time: {error}."
            ))
        })
}

fn check_toon_apps(state: &State, named: &[String]) -> Result<Vec<String>, Error> {
    if named.is_empty() {
        let relay = crate::apps::toon_app(state, None)?;
        return Ok(vec![relay.name.clone()]);
    }
    let mut toon_apps: Vec<String> = Vec::new();
    for name in named {
        crate::apps::toon_app(state, Some(name))?;
        if !toon_apps.contains(name) {
            toon_apps.push(name.clone());
        }
    }
    Ok(toon_apps)
}

fn check_limits(per_command: u64, per_day: u64) -> Result<(), Error> {
    if per_command > per_day {
        return Err(usage(format!(
            "The per-command limit {per_command} is over the per-day limit {per_day}."
        )));
    }
    Ok(())
}

/// Where the supervisor listens for orders: picked once, on loopback, and kept.
fn supervisor_address(record: &mut Record) -> Result<String, Error> {
    if let Some(address) = &record.supervisor {
        return Ok(address.clone());
    }
    let port = ports::kept("127.0.0.1")
        .map_err(|source| failed(ErrorCode::Io, format!("No free port for orders: {source}.")))?;
    let address = format!("http://127.0.0.1:{port}");
    record.supervisor = Some(address.clone());
    Ok(address)
}

pub fn run(home: &Path, command: &AgentKeyCommand) -> Result<Report, Error> {
    match command {
        AgentKeyCommand::Create(args) => create(home, args),
        AgentKeyCommand::List => list(home),
        AgentKeyCommand::Set(args) => set(home, args),
        AgentKeyCommand::Revoke { public_key } => revoke(home, public_key),
    }
}

fn create(home: &Path, args: &AgentKeyCreateArgs) -> Result<Report, Error> {
    let state = state(home)?;
    let scope = parse_scope(&args.scope)?;
    require_passphrase(home, &scope)?;
    if let Some(expires) = &args.expires {
        check_expiry(expires)?;
    }
    let toon_apps = check_toon_apps(&state, &args.toon_app)?;
    if let Some(out) = args.out.as_ref().filter(|out| out.exists()) {
        return Err(usage(format!(
            "{} exists; `toon agent-key create` does not overwrite a file.",
            out.display()
        )));
    }
    // The wallet's limit is the default: an agent key is never given more than the operator
    // allows themself unless they say so.
    let (per_command, per_day) = match (args.max_per_command, args.max_per_day) {
        (Some(command), Some(day)) => (command, day),
        _ => {
            let wallet = spending::read_limits(home)?;
            let clamp = |figure: u128| u64::try_from(figure).unwrap_or(u64::MAX);
            (
                args.max_per_command.unwrap_or(clamp(wallet.per_command)),
                args.max_per_day.unwrap_or(clamp(wallet.per_day)),
            )
        }
    };
    check_limits(per_command, per_day)?;

    let secret = zeroize::Zeroizing::new(keystore::random::<32>()?);
    let public = hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&secret)
            .verifying_key()
            .as_bytes(),
    );
    let key = Key {
        public: public.clone(),
        scope,
        max_per_command: per_command,
        max_per_day: per_day,
        max_restarts: args.max_restarts.unwrap_or(DEFAULT_RESTARTS),
        expires: args.expires.clone(),
        toon_apps,
    };
    let mut record = Record::load(home)?;
    let supervisor = supervisor_address(&mut record)?;
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("agent-key-{}.json", &public[..8])));
    let out = std::path::absolute(&out).unwrap_or(out);
    let surfaces: serde_json::Map<String, Value> = state
        .toon_apps
        .iter()
        .filter(|app| key.toon_apps.contains(&app.name))
        .map(|app| (app.name.clone(), json!(format!("http://{}", app.listen))))
        .collect();
    let rest = json!({
        "public_key": public,
        "supervisor": supervisor,
        "toon_apps": surfaces,
    })
    .to_string();
    // The private key goes straight into a string that is wiped when dropped, not into a
    // `Value`, which would keep a copy that is not.
    let file = zeroize::Zeroizing::new(format!(
        "{{\"private_key\":\"{}\",{}\n",
        zeroize::Zeroizing::new(hex::encode(*secret)).as_str(),
        &rest[1..]
    ));
    let mut written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&out)
        .map_err(|source| failed(ErrorCode::Io, format!("{}: {source}.", out.display())))?;
    if let Err(source) = written
        .write_all(file.as_bytes())
        .and_then(|()| written.sync_all())
    {
        let _ = std::fs::remove_file(&out);
        return Err(failed(
            ErrorCode::Io,
            format!("{}: {source}.", out.display()),
        ));
    }
    let before = record.keys.clone();
    record.keys.push(key.clone());
    let saved = record.save(home).and_then(|()| write_lists(home, &state));
    if let Err(error) = saved {
        // Nobody holds the key once its file is gone, so it is taken off the record and
        // every list again rather than left granted.
        let _ = std::fs::remove_file(&out);
        record.keys = before;
        let _ = record.save(home).and_then(|()| write_lists(home, &state));
        return Err(error);
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "agent_key": key.json(), "key_file": out.display().to_string() }),
        text: format!(
            "Agent key {public} created, granted on {}. Its key file, {}, holds the private key: \
             hand it to the agent and keep no copy where the agent can read it.",
            key.toon_apps.join(", "),
            out.display()
        ),
    })
}

fn describe(key: &Key) -> String {
    format!(
        "{}  scope: {}  limit: {} per command, {} per day  restarts: {} per day  expires: {}  toon apps: {}",
        key.public,
        key.scope.join(","),
        key.max_per_command,
        key.max_per_day,
        key.max_restarts,
        key.expires.as_deref().unwrap_or("never"),
        key.toon_apps.join(","),
    )
}

fn list(home: &Path) -> Result<Report, Error> {
    let record = Record::load(home)?;
    let text = if record.keys.is_empty() {
        "No agent keys.".to_owned()
    } else {
        record
            .keys
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "agent_keys": record.keys.iter().map(Key::json).collect::<Vec<_>>() }),
        text,
    })
}

fn set(home: &Path, args: &AgentKeySetArgs) -> Result<Report, Error> {
    let state = state(home)?;
    let mut record = Record::load(home)?;
    let index = record.find(&args.public_key)?;
    let mut key = record.keys[index].clone();
    if !args.scope.is_empty() {
        key.scope = parse_scope(&args.scope)?;
    }
    require_passphrase(home, &key.scope)?;
    if let Some(expires) = &args.expires {
        check_expiry(expires)?;
        key.expires = Some(expires.clone());
    }
    if args.no_expiry {
        key.expires = None;
    }
    if !args.toon_app.is_empty() {
        key.toon_apps = check_toon_apps(&state, &args.toon_app)?;
    }
    key.max_per_command = args.max_per_command.unwrap_or(key.max_per_command);
    key.max_per_day = args.max_per_day.unwrap_or(key.max_per_day);
    key.max_restarts = args.max_restarts.unwrap_or(key.max_restarts);
    check_limits(key.max_per_command, key.max_per_day)?;
    record.keys[index] = key.clone();
    record.save(home)?;
    write_lists(home, &state)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "agent_key": key.json() }),
        text: format!("Agent key changed: {}", describe(&key)),
    })
}

fn revoke(home: &Path, public: &str) -> Result<Report, Error> {
    let state = state(home)?;
    let mut record = Record::load(home)?;
    let index = record.find(public)?;
    record.keys.remove(index);
    record.save(home)?;
    write_lists(home, &state)?;
    crate::pending::drop_key(home, public)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "revoked": public }),
        text: format!(
            "Agent key {public} revoked. Each connector refuses it from its next request."
        ),
    })
}
