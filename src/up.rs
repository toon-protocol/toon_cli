//! `toon up`: the supervisor, in the foreground.
//!
//! It renders each TOON app's connector config from the agent node's state, starts the
//! connector as a child process of this same binary (`connector`), and serves lifecycle
//! requests on a local socket (`control`): `status` asks what runs, and `down` stops the
//! connector and then this process.

use std::env;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::Digest;

use crate::connector::{self, Startup};
use crate::control;
use crate::funding;
use crate::node::{self, AppFiles, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::runner::{self, AppRunner, AppSpec, RunningApp};

/// How long a connector gets to exit once its supervisor is stopping, before it is killed.
const GRACE: Duration = Duration::from_secs(10);

/// How often a supervisor asks whether its apps still run. Asking a container is a
/// `docker` process, so not on every tick.
const APPS_EVERY: Duration = Duration::from_secs(1);

/// An app behind the connector, as the control socket reports it.
struct AppStatus {
    name: String,
    address: SocketAddr,
    running: AtomicBool,
}

/// What the control socket reports of the running connector.
struct Shared {
    pid: u32,
    address: String,
    toon_app: String,
    apps: Vec<AppStatus>,
    stop: AtomicBool,
    running: AtomicBool,
}

/// A supervisor whose connector is listening.
pub struct Supervisor {
    connector: Child,
    /// The apps behind it, stopped after it.
    apps: Vec<Box<dyn RunningApp>>,
    /// The connector exits when this closes, so it is held for as long as the
    /// supervisor lives and is never written to.
    alive: Option<ChildStdin>,
    shared: Arc<Shared>,
    socket: PathBuf,
    home: PathBuf,
    log: PathBuf,
}

/// How a supervisor ended.
pub enum Stopped {
    /// `toon down` asked it to.
    Down,
    /// The connector stopped by itself.
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

/// An app that has been started, by name.
type Started = Vec<(String, Box<dyn RunningApp>)>;

/// Start the apps behind `app`'s connector. What it started is stopped again if one fails.
fn start_apps(home: &Path, app: &ToonApp, runner: &dyn AppRunner) -> Result<Started, Error> {
    let mut started = Started::new();
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
    apps: &mut Started,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    let relay = apps
        .iter()
        .find(|(name, _)| name == node::RELAY)
        .map(|(_, running)| running.write_address());
    let files = node::render(home, app, relay)?;
    let log = files.log;
    let logs = File::options()
        .create(true)
        .append(true)
        .open(&log)
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
            let shared = Arc::new(Shared {
                pid: child.id(),
                address,
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
                running: AtomicBool::new(true),
            });
            let answering = Arc::clone(&shared);
            thread::spawn(move || control::serve(listener, |request| answering.answer(request)));
            return Ok(Supervisor {
                connector: child,
                apps: apps.drain(..).map(|(_, running)| running).collect(),
                alive: Some(alive),
                shared,
                socket: socket.to_path_buf(),
                home: home.to_path_buf(),
                log,
            });
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
            "status" => json!({
                "toon_apps": [{
                    "name": self.toon_app,
                    "connector": {
                        "address": self.address,
                        "pid": self.pid,
                        "running": self.running.load(Ordering::SeqCst),
                    },
                    "apps": self.apps.iter().map(|app| json!({
                        "name": app.name,
                        "address": app.address.to_string(),
                        "running": app.running.load(Ordering::SeqCst),
                    })).collect::<Vec<_>>(),
                }],
            }),
            "down" => {
                self.stop.store(true, Ordering::SeqCst);
                json!({ "stopping": true })
            }
            _ => json!({ "error": "unknown request" }),
        }
    }
}

impl Supervisor {
    /// What is running.
    pub fn report(&self) -> Report {
        let pid = self.connector.id();
        let address = &self.shared.address;
        let revision = connector::REVISION;
        let home = self.home.to_string_lossy();
        let log = self.log.to_string_lossy();
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

    /// Stay in the foreground until `toon down` or until the connector stops. A
    /// supervisor has nothing else to wait for, so a connector that stops by itself is a
    /// failure.
    pub fn wait(mut self) -> Stopped {
        let mut apps_checked = Instant::now();
        let stopped = loop {
            if self.shared.stop.load(Ordering::SeqCst) {
                break self.stop_connector();
            }
            let stopped_app = if apps_checked.elapsed() >= APPS_EVERY {
                apps_checked = Instant::now();
                self.stopped_app()
            } else {
                None
            };
            if let Some(stopped) = stopped_app {
                let _ = self.stop_connector();
                break Stopped::Failed(Error {
                    code: ErrorCode::AppFailed,
                    message: format!("The app {stopped} stopped."),
                });
            }
            match self.connector.try_wait() {
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Ok(Some(status)) => {
                    break Stopped::Failed(failed(format!("The connector stopped ({status}).")))
                }
                Err(error) => {
                    break Stopped::Failed(failed(format!("The connector stopped ({error}).")))
                }
            }
        };
        // The apps go once the connector that delivers to them has.
        for app in &mut self.apps {
            app.stop();
        }
        self.shared.running.store(false, Ordering::SeqCst);
        // Last, so that a `toon down` that sees the socket gone knows everything has.
        let _ = std::fs::remove_file(&self.socket);
        stopped
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
    fn stop_connector(&mut self) -> Stopped {
        drop(self.alive.take());
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if matches!(self.connector.try_wait(), Ok(Some(_))) {
                return Stopped::Down;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.connector.kill();
        let _ = self.connector.wait();
        Stopped::Down
    }
}
