//! The supervisor, in the foreground: what `toon up --foreground` runs, and what the
//! `systemd --user` unit (`service`) runs.
//!
//! It renders each TOON app's connector config from the agent node's state, starts the
//! connector as a child process of this same binary (`connector`), restarts it when it
//! exits, and serves lifecycle requests on a local socket (`control`): `status` asks what
//! runs, and `down` stops the connector and then this process.

use std::env;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::Digest;

use crate::connector::{self, Startup};
use crate::control;
use crate::funding;
use crate::node::{self, AppFiles, ConnectorFiles, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::runner::{self, AppRunner, AppSpec, RunningApp};

/// How long a connector gets to exit once its supervisor is stopping, before it is killed.
const GRACE: Duration = Duration::from_secs(10);

/// How often a supervisor asks whether its apps still run. Asking a container is a
/// `docker` process, so not on every tick.
const APPS_EVERY: Duration = Duration::from_secs(1);

/// How long the supervisor waits before it restarts a connector that has just stopped,
/// and the longest it waits between attempts that keep failing.
const FIRST_RESTART_DELAY: Duration = Duration::from_millis(250);
const LONGEST_RESTART_DELAY: Duration = Duration::from_secs(30);

/// A connector that ran this long was healthy, so its next restart starts over at the
/// shortest delay.
const HEALTHY: Duration = Duration::from_secs(60);

/// An app behind the connector, as the control socket reports it.
struct AppStatus {
    name: String,
    address: SocketAddr,
    running: AtomicBool,
}

/// What the control socket reports of the connector.
struct Live {
    pid: Option<u32>,
    address: Option<String>,
    running: bool,
    /// How many times the supervisor has started the connector again.
    restarts: u32,
    /// Why the connector last stopped, if it has.
    last_exit: Option<String>,
}

struct Shared {
    toon_app: String,
    apps: Vec<AppStatus>,
    stop: AtomicBool,
    live: Mutex<Live>,
}

/// A connector that is listening.
struct Started {
    child: Child,
    /// The connector exits when this closes, so it is held for as long as the connector
    /// is wanted and is never written to.
    alive: ChildStdin,
    address: String,
    since: Instant,
}

/// A supervisor whose connector is listening.
pub struct Supervisor {
    connector: Option<Started>,
    /// The apps behind it, stopped after it.
    apps: Vec<Box<dyn RunningApp>>,
    files: ConnectorFiles,
    shared: Arc<Shared>,
    socket: PathBuf,
    home: PathBuf,
    /// What the first start reported, for `report`.
    first: (u32, String),
    /// How long to wait before the next restart.
    delay: Duration,
}

/// How a supervisor ended.
pub enum Stopped {
    /// `toon down` asked it to.
    Down,
    /// An app behind the connector stopped by itself.
    Failed(Error),
}

fn failed(message: String) -> Error {
    Error {
        code: ErrorCode::ConnectorFailed,
        message,
    }
}

/// Start the connector of the agent node at `home`, and return once it is listening.
pub fn start(home: &Path) -> Result<Supervisor, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    // One connector per supervisor until a command creates a second TOON app.
    let app = &state.toon_apps[0];
    // A connector whose settlement key is empty is not started: it would only fail
    // later, and not say why.
    // A chain that cannot be asked is not a verdict: the connector binds to its chain
    // before it listens, and refuses with its own reason if the chain is not there.
    // A settlement key that cannot be read is.
    if let Ok(lacking) = funding::shortfalls(funding::needs(home, app)?) {
        if !lacking.is_empty() {
            return Err(funding::unfunded(state.network, &lacking));
        }
    }
    let Some(listener) = control::bind(home).map_err(|error| Error {
        code: ErrorCode::Io,
        message: format!("{}: {error}.", control::path(home).display()),
    })?
    else {
        return Err(Error {
            code: ErrorCode::AlreadyRunning,
            message: format!(
                "A supervisor is already running this agent node, at {}.",
                control::path(home).display()
            ),
        });
    };
    let socket = control::path(home);
    match launch(home, app, &*runner::from_environment(), listener, &socket) {
        Ok(supervisor) => Ok(supervisor),
        Err(error) => {
            let _ = std::fs::remove_file(&socket);
            Err(error)
        }
    }
}

/// The apps that have been started, by name.
type StartedApps = Vec<(String, Box<dyn RunningApp>)>;

/// Start the apps behind `app`'s connector. What it started is stopped again if one fails.
fn start_apps(home: &Path, app: &ToonApp, runner: &dyn AppRunner) -> Result<StartedApps, Error> {
    let mut started = StartedApps::new();
    for name in &app.apps {
        let files = AppFiles::of(home, name);
        let identity = fs::read(&files.identity_key).map_err(|error| Error {
            code: ErrorCode::AppFailed,
            message: format!(
                "The identity key of the app {name} is not readable at {}: {error}.",
                files.identity_key.display()
            ),
        })?;
        let spec = AppSpec {
            instance: format!("{}-{name}", instance(home)),
            image: env!("TOON_RELAY_IMAGE").to_owned(),
            // The relay's own Nostr identity is the wallet's, in hex.
            env: vec![("NOSTR_SECRET_KEY".into(), hex::encode(identity))],
            data_dir: files.data_dir,
        };
        match runner.start(&spec) {
            Ok(running) => started.push((name.clone(), running)),
            Err(error) => {
                for (_, running) in &mut started {
                    running.stop();
                }
                return Err(error);
            }
        }
    }
    Ok(started)
}

/// A name for the agent node at `home` that is the same every time and differs between
/// homes, so that a container left by a dead supervisor is found and replaced.
fn instance(home: &Path) -> String {
    let digest = sha2::Sha256::digest(home.to_string_lossy().as_bytes());
    hex::encode(&digest[..6])
}

fn launch(
    home: &Path,
    app: &ToonApp,
    runner: &dyn AppRunner,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    let mut apps = start_apps(home, app, runner)?;
    let result = launch_connector(home, app, &mut apps, listener, socket);
    if result.is_err() {
        for (_, running) in &mut apps {
            running.stop();
        }
    }
    result
}

fn launch_connector(
    home: &Path,
    app: &ToonApp,
    apps: &mut StartedApps,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    let relay = apps
        .iter()
        .find(|(name, _)| name == node::RELAY)
        .map(|(_, running)| running.write_address());
    let files = node::render(home, app, relay)?;
    let started = spawn(&files)?;
    let first = (started.child.id(), started.address.clone());
    let shared = Arc::new(Shared {
        toon_app: app.name.clone(),
        apps: apps
            .iter()
            .map(|(name, running)| AppStatus {
                name: name.clone(),
                address: running.write_address(),
                running: AtomicBool::new(true),
            })
            .collect(),
        stop: AtomicBool::new(false),
        live: Mutex::new(Live {
            pid: Some(first.0),
            address: Some(first.1.clone()),
            running: true,
            restarts: 0,
            last_exit: None,
        }),
    });
    let answering = Arc::clone(&shared);
    thread::spawn(move || control::serve(listener, |request| answering.answer(request)));
    Ok(Supervisor {
        connector: Some(started),
        apps: apps.drain(..).map(|(_, running)| running).collect(),
        files,
        shared,
        socket: socket.to_path_buf(),
        home: home.to_path_buf(),
        first,
        delay: FIRST_RESTART_DELAY,
    })
}

/// Start the connector described by `files` as a child process, and return once it is
/// listening or has said why it is not.
fn spawn(files: &ConnectorFiles) -> Result<Started, Error> {
    let log = &files.log;
    let logs = File::options()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|error| failed(format!("The connector's log could not be opened: {error}.")))?;
    let toon = env::current_exe()
        .map_err(|error| failed(format!("This binary could not find itself: {error}.")))?;
    let mut child = Command::new(toon)
        .arg(connector::COMMAND)
        .arg(&files.config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(logs)
        .spawn()
        .map_err(|error| failed(format!("The connector could not be started: {error}.")))?;
    let alive = child.stdin.take().expect("the connector's stdin is piped");
    let stdout = child
        .stdout
        .take()
        .expect("the connector's stdout is piped");

    let mut line = String::new();
    // An unreadable or empty line is a connector that died without saying why.
    let _ = BufReader::new(stdout).read_line(&mut line);
    let refusal = match Startup::heard(&line) {
        Some(Startup::Listening(address)) => {
            return Ok(Started {
                child,
                alive,
                address,
                since: Instant::now(),
            })
        }
        Some(Startup::Refused(why)) => format!("The connector refused to start: {why}"),
        None => format!(
            "The connector exited before it was listening. Its log is {}.",
            log.display()
        ),
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(failed(refusal))
}

impl Shared {
    fn answer(&self, request: &str) -> Value {
        match request {
            "status" => {
                let live = self.live();
                json!({
                    "toon_apps": [{
                        "name": self.toon_app,
                        "connector": {
                            "address": live.address,
                            "pid": live.pid,
                            "running": live.running,
                            "restarts": live.restarts,
                            "last_exit": live.last_exit,
                        },
                        "apps": self.apps.iter().map(|app| json!({
                            "name": app.name,
                            "address": app.address.to_string(),
                            "running": app.running.load(Ordering::SeqCst),
                        })).collect::<Vec<_>>(),
                    }],
                })
            }
            "down" => {
                self.stop.store(true, Ordering::SeqCst);
                json!({ "stopping": true })
            }
            _ => json!({ "error": "unknown request" }),
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().expect("the connector's state")
    }
}

impl Supervisor {
    /// What is running.
    pub fn report(&self) -> Report {
        let (pid, address) = &self.first;
        let revision = connector::REVISION;
        let home = self.home.to_string_lossy();
        let log = self.files.log.to_string_lossy();
        let socket = self.socket.to_string_lossy();
        Report {
            exit: Exit::Success,
            json: json!({
                "home": home,
                "connector": {
                    "address": address,
                    "pid": pid,
                    "revision": revision,
                    "log": log,
                },
                "socket": socket,
            }),
            text: format!(
                "Connector listening on {address} (pid {pid}, revision {revision}). \
                 Its log is {log}."
            ),
        }
    }

    /// Stay in the foreground until `toon down`, or until an app stops. A connector that
    /// stops by itself is started again, after a delay that doubles while the starts keep
    /// failing. An app that stops is not: the supervisor stops with it.
    pub fn wait(mut self) -> Stopped {
        let mut apps_checked = Instant::now();
        let mut stopped = Stopped::Down;
        while !self.shared.stop.load(Ordering::SeqCst) {
            if apps_checked.elapsed() >= APPS_EVERY {
                apps_checked = Instant::now();
                if let Some(app) = self.stopped_app() {
                    stopped = Stopped::Failed(Error {
                        code: ErrorCode::AppFailed,
                        message: format!("The app {app} stopped."),
                    });
                    break;
                }
            }
            let Some(started) = self.connector.as_mut() else {
                self.restart();
                continue;
            };
            match started.child.try_wait() {
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Ok(Some(status)) => self.stopped(format!("The connector stopped ({status}).")),
                Err(error) => self.stopped(format!("The connector stopped ({error}).")),
            }
        }
        self.stop_connector();
        // The apps go once the connector that delivers to them has.
        for app in &mut self.apps {
            app.stop();
        }
        self.shared.live().running = false;
        // Last, so that a `toon down` that sees the socket gone knows everything has.
        let _ = std::fs::remove_file(&self.socket);
        stopped
    }

    /// The connector is gone: note why, and let the next turn of `wait` restart it.
    fn stopped(&mut self, why: String) {
        if let Some(started) = self.connector.take() {
            eprintln!("{why}");
            let mut live = self.shared.live();
            live.running = false;
            live.pid = None;
            live.address = None;
            live.last_exit = Some(why);
            drop(live);
            drop(started.alive);
            // A connector that ran for a while was not crash-looping.
            if started.since.elapsed() >= HEALTHY {
                self.delay = FIRST_RESTART_DELAY;
            }
        }
    }

    /// Wait out the delay, then start the connector again. A start that fails leaves it
    /// down, and `wait` comes back for another go after a longer delay.
    fn restart(&mut self) {
        let until = Instant::now() + self.delay;
        while Instant::now() < until {
            if self.shared.stop.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.delay = (self.delay * 2).min(LONGEST_RESTART_DELAY);
        match spawn(&self.files) {
            Ok(started) => {
                let mut live = self.shared.live();
                live.pid = Some(started.child.id());
                live.address = Some(started.address.clone());
                live.running = true;
                live.restarts += 1;
                drop(live);
                self.connector = Some(started);
            }
            Err(error) => {
                eprintln!("{}", error.message);
                self.shared.live().last_exit = Some(error.message);
            }
        }
    }

    /// The name of an app that has stopped, if one has. Each app's status is kept current.
    fn stopped_app(&mut self) -> Option<String> {
        let mut stopped = None;
        for (running, status) in self.apps.iter_mut().zip(&self.shared.apps) {
            let alive = running.running();
            status.running.store(alive, Ordering::SeqCst);
            if !alive && stopped.is_none() {
                stopped = Some(status.name.clone());
            }
        }
        stopped
    }

    /// Close the connector's stdin, which it takes as its cue to exit, and kill it if
    /// it does not.
    fn stop_connector(&mut self) {
        let Some(mut started) = self.connector.take() else {
            return;
        };
        drop(started.alive);
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if matches!(started.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = started.child.kill();
        let _ = started.child.wait();
    }
}
