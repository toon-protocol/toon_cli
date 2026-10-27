//! The supervisor as the tip route's app (ADR 0013).
//!
//! While the state has a tip route, the supervisor listens on its loopback port, and the
//! connector delivers each tip there: the body is the opened payload, the tip receipt (kind
//! 9850), `X-TOON-Amount` is what arrived and `X-TOON-Chain` names the chain it arrived on.
//! The tip is fulfilled whatever the handler answers (connector ADR 0019), so the handler
//! always answers success. It queues a tip acknowledgement (kind 9851) only for a receipt
//! that is signed, names this agent identity, states the unit of the chain it arrived on and
//! has not been acknowledged before; any other tip is logged with the reason and kept nowhere.
//!
//! An acknowledgement is published to the agent node's own relay and to the relay the
//! network profile follows. One that cannot be published stays in `tips.json` in the home,
//! and is tried again after a delay that doubles up to a cap, across restarts. The ids of the
//! receipts already acknowledged are kept in the same file, so a restart does not
//! acknowledge a receipt twice.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use serde_json::{json, Value};

use crate::event;
use crate::inbox;
use crate::node::{self, State};
use crate::outcome::Error;

/// The kind of a tip receipt, the payer's word about a tip.
pub const RECEIPT: u64 = 9850;

/// The kind of a tip acknowledgement, the recipient's.
pub const ACKNOWLEDGEMENT: u64 = 9851;

/// How often the worker looks for a tip route to listen for and an acknowledgement to publish.
const TICK: Duration = Duration::from_millis(250);

/// How long a failed publication waits before it is tried again, doubling with each failure
/// up to `LONGEST_WAIT`.
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(300);

/// The target of an acknowledgement that is the agent node's own relay. Any other is the URL of
/// a relay that is paid.
const OWN: &str = "own";

/// Held while the store is read and written, so that the handler and the worker do not each
/// save what they read before the other's change.
static STORE: Mutex<()> = Mutex::new(());

/// Where the pending acknowledgements and the acknowledged receipts are kept.
pub fn store_path(home: &Path) -> PathBuf {
    home.join("tips.json")
}

/// An acknowledgement that has not reached every relay it goes to.
#[derive(Clone, Debug)]
struct Pending {
    event: Value,
    /// The relays it has still to reach: `OWN`, or a relay's URL.
    targets: Vec<String>,
    failures: u32,
    /// Unix milliseconds before which it is not tried.
    retry_at: u64,
    last_error: Option<String>,
}

impl Pending {
    fn json(&self) -> Value {
        json!({
            "event": self.event,
            "targets": self.targets,
            "failures": self.failures,
            "retry_at": self.retry_at,
            "last_error": self.last_error,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            event: value["event"].clone(),
            targets: value["targets"]
                .as_array()?
                .iter()
                .map(|target| target.as_str().map(str::to_owned))
                .collect::<Option<_>>()?,
            failures: u32::try_from(value["failures"].as_u64()?).ok()?,
            retry_at: value["retry_at"].as_u64()?,
            last_error: value["last_error"].as_str().map(str::to_owned),
        })
    }

    fn id(&self) -> &str {
        self.event["id"].as_str().unwrap_or_default()
    }
}

#[derive(Debug, Default)]
struct Store {
    /// The ids of the receipts that were acknowledged, or queued to be.
    acknowledged: Vec<String>,
    pending: Vec<Pending>,
}

impl Store {
    /// The store in `home`. One that is missing or not readable holds nothing.
    fn load(home: &Path) -> Self {
        let Some(value) = std::fs::read(store_path(home))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return Self::default();
        };
        Self {
            acknowledged: value["acknowledged"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect(),
            pending: value["pending"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Pending::from_json)
                .collect(),
        }
    }

    fn save(&self, home: &Path) -> Result<(), Error> {
        let value = json!({
            "acknowledged": self.acknowledged,
            "pending": self.pending.iter().map(Pending::json).collect::<Vec<_>>(),
        });
        node::write(&store_path(home), format!("{value}\n").as_bytes(), 0o600)
    }
}

/// What `toon status` says of the acknowledgements.
pub struct Queue {
    /// How many acknowledgements have not reached every relay.
    pub waiting: usize,
    /// Why the newest of them failed last.
    pub last_error: Option<String>,
}

/// The acknowledgements that wait, read from the home.
pub fn queue(home: &Path) -> Queue {
    let store = Store::load(home);
    Queue {
        waiting: store.pending.len(),
        last_error: store
            .pending
            .iter()
            .filter_map(|pending| pending.last_error.clone())
            .next_back(),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The tags named `name` of `event`.
fn tags<'a>(event: &'a Value, name: &'a str) -> impl Iterator<Item = &'a Vec<Value>> {
    event["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_array())
        .filter(move |tag| tag.first().and_then(Value::as_str) == Some(name))
}

/// What the connector said arrived with a tip.
pub struct Arrived<'a> {
    /// `X-TOON-Amount`, in base units.
    pub amount: Option<&'a str>,
    /// `X-TOON-Chain`.
    pub chain: Option<&'a str>,
}

/// The unit of the chain a tip arrived on: `evm` is the TOON app's EVM token, whose code and
/// scale `node::Evm::unit` answers. Any other chain, or a token with no stated code, has none.
fn chain_unit(app: &node::ToonApp, chain: &str) -> Result<node::Unit, String> {
    let family = chain
        .split(':')
        .next()
        .unwrap_or(chain)
        .to_ascii_lowercase();
    match family.as_str() {
        "evm" => app
            .evm
            .as_ref()
            .and_then(node::Evm::unit)
            .ok_or_else(|| format!("the token of the chain {chain} has no asset code")),
        _ => Err(format!(
            "the chain {chain} has no unit this agent node knows"
        )),
    }
}

/// Take a tip that the connector delivered: queue its acknowledgement, or say why not. The
/// `Ok` says what was done, for the supervisor's log.
pub fn receive(home: &Path, arrived: &Arrived, body: &[u8]) -> Result<String, String> {
    let state = State::load(home)
        .map_err(|error| error.message)?
        .ok_or("there is no agent node")?;
    let app = state
        .toon_apps
        .iter()
        .find(|app| app.tip.is_some())
        .ok_or("tips are not enabled")?;
    let secret = inbox::kept_secret(home).ok_or("the agent identity's secret is not kept")?;
    let identity = event::public_key(&secret).map_err(|error| error.message)?;

    let receipt: Value =
        serde_json::from_slice(body).map_err(|error| format!("the body is not JSON: {error}"))?;
    if receipt["kind"] != RECEIPT {
        return Err(format!("the body is not a tip receipt (kind {RECEIPT})"));
    }
    event::verify(&receipt).map_err(|why| format!("the tip receipt is not signed: {why}"))?;
    let for_us: Vec<&str> = tags(&receipt, "p")
        .filter_map(|tag| tag.get(1).and_then(Value::as_str))
        .collect();
    if for_us.len() != 1 || !for_us[0].eq_ignore_ascii_case(&identity) {
        return Err("the tip receipt is not for this agent identity alone".into());
    }
    let amount = arrived.amount.ok_or("the tip came with no X-TOON-Amount")?;
    if amount.is_empty() || !amount.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("X-TOON-Amount is not a number: {amount}"));
    }
    let chain = arrived.chain.ok_or("the tip came with no X-TOON-Chain")?;
    let unit = chain_unit(app, chain)?;
    let stated = tags(&receipt, "amount")
        .next()
        .filter(|tag| tag.len() >= 4)
        .ok_or("the tip receipt states no amount, code and scale")?;
    let (code, scale) = (stated[2].as_str(), stated[3].as_str());
    if code != Some(unit.code.as_str()) || scale != Some(unit.decimals.to_string().as_str()) {
        return Err(format!(
            "the tip receipt states {} {} but {chain} is {} {}",
            code.unwrap_or("no code"),
            scale.unwrap_or("no scale"),
            unit.code,
            unit.decimals
        ));
    }
    let id = receipt["id"].as_str().unwrap_or_default().to_owned();

    let acknowledgement = acknowledgement_of(&receipt, &secret, amount, &unit)?;
    let _held = STORE.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = Store::load(home);
    if store.acknowledged.contains(&id) {
        return Err(format!("the tip receipt {id} was acknowledged before"));
    }
    let mut targets = vec![OWN.to_owned()];
    targets.extend(state.relay_url.clone());
    store.acknowledged.push(id.clone());
    store.pending.push(Pending {
        event: acknowledgement,
        targets,
        failures: 0,
        retry_at: 0,
        last_error: None,
    });
    store.save(home).map_err(|error| error.message)?;
    Ok(format!(
        "queued the acknowledgement of the tip receipt {id}"
    ))
}

fn acknowledgement_of(
    receipt: &Value,
    secret: &[u8; 32],
    amount: &str,
    unit: &node::Unit,
) -> Result<Value, String> {
    let mut tags: Vec<Value> = ["e", "a"]
        .iter()
        .flat_map(|name| self::tags(receipt, name))
        .map(|tag| json!(tag))
        .collect();
    tags.push(json!([
        "amount",
        amount,
        unit.code,
        unit.decimals.to_string()
    ]));
    tags.push(json!(["receipt", receipt["id"]]));
    if self::tags(receipt, "anon").next().is_none() {
        tags.push(json!(["p", receipt["pubkey"]]));
    }
    event::sign(
        secret,
        event::now(),
        ACKNOWLEDGEMENT,
        Value::Array(tags),
        "",
    )
    .map_err(|error| error.message)
}

/// Publish `event` to `target`: the agent node's own relay, or the relay at that URL.
fn publish_to(home: &Path, target: &str, event: &Value) -> Result<(), String> {
    let report = if target == OWN {
        event::write(home, event.clone(), 0)
    } else {
        event::publish_signed_to(home, target, event.clone())
    }
    .map_err(|error| error.message)?;
    if report.exit == crate::outcome::Exit::Success {
        Ok(())
    } else {
        Err(report.text)
    }
}

/// Try every acknowledgement that is due once.
fn drain(home: &Path) {
    let due: Vec<Pending> = {
        let _held = STORE.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_millis();
        Store::load(home)
            .pending
            .into_iter()
            .filter(|pending| pending.retry_at <= now)
            .collect()
    };
    for pending in due {
        // The relays are asked one at a time and none is waited for twice.
        let mut reached = Vec::new();
        let mut failed: Option<String> = None;
        for target in &pending.targets {
            match publish_to(home, target, &pending.event) {
                Ok(()) => reached.push(target.clone()),
                Err(why) => {
                    eprintln!(
                        "The tip acknowledgement {} could not be published to {target}: {why}",
                        pending.id()
                    );
                    failed = Some(why);
                }
            }
        }
        let _held = STORE.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = Store::load(home);
        let Some(position) = store
            .pending
            .iter()
            .position(|kept| kept.id() == pending.id())
        else {
            continue;
        };
        let kept = &mut store.pending[position];
        kept.targets.retain(|target| !reached.contains(target));
        if kept.targets.is_empty() {
            store.pending.remove(position);
        } else {
            kept.failures = pending.failures.saturating_add(1);
            let wait = FIRST_WAIT
                .saturating_mul(1u32 << pending.failures.min(16))
                .min(LONGEST_WAIT);
            kept.retry_at = now_millis() + wait.as_millis() as u64;
            kept.last_error = failed;
        }
        if let Err(error) = store.save(home) {
            eprintln!(
                "The tip acknowledgements could not be kept: {}",
                error.message
            );
        }
    }
}

/// The listener on the tip route's loopback port.
struct Listener {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    fn bind(home: &Path, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (home, stop) = (home.to_path_buf(), Arc::clone(&stop));
            thread::spawn(move || serve(&home, listener, &stop))
        };
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(home: &Path, listener: TcpListener, stop: &Arc<AtomicBool>) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return;
    };
    let home = home.to_path_buf();
    let router = Router::new().fallback(move |headers: HeaderMap, body: Bytes| {
        let home = home.clone();
        async move {
            let handled = tokio::task::spawn_blocking(move || {
                let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
                let arrived = Arrived {
                    amount: header("x-toon-amount"),
                    chain: header("x-toon-chain"),
                };
                match receive(&home, &arrived, &body) {
                    Ok(done) => eprintln!("Tip: {done}."),
                    Err(why) => eprintln!("Tip not acknowledged: {why}."),
                }
            })
            .await;
            if handled.is_err() {
                eprintln!("Tip not acknowledged: the handler failed.");
            }
            // The tip is fulfilled whatever is answered: the money has arrived.
            StatusCode::OK.into_response()
        }
    });
    let stop = Arc::clone(stop);
    runtime.block_on(async move {
        let Ok(server) = axum::Server::from_tcp(listener) else {
            return;
        };
        let _ = server
            .serve(router.into_make_service())
            .with_graceful_shutdown(async move {
                while !stop.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
    });
}

/// The worker that listens for tips and publishes their acknowledgements; it stops when it is
/// dropped.
pub struct Tips {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tips {
    pub fn start(home: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut listener = None;
        // The first look is made before the connectors start, so that the route they are
        // given has its app listening.
        listen(home, &mut listener);
        let thread = {
            let (home, stop) = (home.to_path_buf(), Arc::clone(&stop));
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    listen(&home, &mut listener);
                    drain(&home);
                    thread::sleep(TICK);
                }
            })
        };
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Tips {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Listen where the state's tip route is, and nowhere else.
fn listen(home: &Path, listener: &mut Option<Listener>) {
    let wanted = State::load(home)
        .ok()
        .flatten()
        .and_then(|state| state.toon_apps.iter().find_map(|app| app.tip))
        .map(|tip| tip.port);
    if listener.as_ref().map(|bound| bound.port) == wanted {
        return;
    }
    *listener = None;
    if let Some(port) = wanted {
        match Listener::bind(home, port) {
            Ok(bound) => *listener = Some(bound),
            Err(error) => eprintln!("The tip route's port {port} could not be bound: {error}."),
        }
    }
}
