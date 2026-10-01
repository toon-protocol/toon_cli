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
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::Digest;

use crate::connector::{self, Startup};
use crate::control;
use crate::funding;
use crate::node::{self, App, AppFiles, ConnectorFiles, Reach, Source, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::overlay::{self, Edge};
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

/// Why a reload failed, and whether the connector was stopped on the way: a reload that
/// fails before it stops the connector leaves it as it was.
struct Unreloaded {
    error: Error,
    stopped: bool,
}

/// Someone waiting for the connector to be started again with the state as it now is.
type Reload = Sender<Result<(), Unreloaded>>;

struct Shared {
    toon_app: String,
    apps: Mutex<Vec<AppStatus>>,
    /// The `reload` requests that `wait` has not carried out yet.
    reload: Mutex<Vec<Reload>>,
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

/// The overlay a hidden service's connector is reached and reaches out through.
struct Hidden {
    edge: Box<dyn Edge>,
    endpoint: String,
    /// Where the relay's read port is, if the TOON app fronts a relay.
    read: Option<SocketAddr>,
}

impl Hidden {
    /// Publish the connector, now listening at `address`, and the relay's read port, at the
    /// onion endpoint.
    fn publish(&self, address: &str) {
        let mut ports = Vec::new();
        if let Ok(address) = address.parse() {
            ports.push((overlay::CONNECTOR_PORT, address));
        }
        if let Some(read) = self.read {
            ports.push((overlay::RELAY_READ_PORT, read));
        }
        self.edge.publish(&self.endpoint, &ports);
    }

    fn overlay(&self) -> node::Overlay {
        node::Overlay {
            proxy: self.edge.proxy(),
            endpoint: self.endpoint.clone(),
        }
    }
}

/// Where the relay's read port is, if it runs.
fn read_address(apps: &StartedApps) -> Option<SocketAddr> {
    apps.iter()
        .find(|(name, _)| name == node::RELAY)
        .and_then(|(_, running)| running.read_address())
}

/// A supervisor whose connector is listening.
pub struct Supervisor {
    /// Held for as long as the connector runs, and dropped once it and the apps are stopped.
    hidden: Option<Hidden>,
    connector: Option<Started>,
    /// The apps behind it that run, stopped after it.
    apps: StartedApps,
    runner: Box<dyn AppRunner>,
    /// The TOON app as the connector now runs it.
    app: ToonApp,
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
    match launch(home, app, runner::from_environment(), listener, &socket) {
        Ok(supervisor) => Ok(supervisor),
        Err(error) => {
            let _ = std::fs::remove_file(&socket);
            Err(error)
        }
    }
}

/// The apps that have been started, by name.
type StartedApps = Vec<(String, Box<dyn RunningApp>)>;

/// Start one app, if it is one the supervisor runs, and return once it is healthy.
fn start_app(
    home: &Path,
    toon: &ToonApp,
    app: &App,
    runner: &dyn AppRunner,
) -> Result<Option<Box<dyn RunningApp>>, Error> {
    let files = AppFiles::of(home, &app.name);
    let (image, env) = match &app.source {
        Source::Url(_) => return Ok(None),
        // The relay's own Nostr identity is the wallet's, in hex.
        Source::Relay => {
            let identity = fs::read(&files.identity_key).map_err(|error| Error {
                code: ErrorCode::AppFailed,
                message: format!(
                    "The identity key of the app {} is not readable at {}: {error}.",
                    app.name,
                    files.identity_key.display()
                ),
            })?;
            let mut env = vec![("NOSTR_SECRET_KEY".to_owned(), hex::encode(identity))];
            env.extend(toon.relay.env());
            (env!("TOON_RELAY_IMAGE").to_owned(), env)
        }
        Source::Image(image) => (image.clone(), Vec::new()),
    };
    let spec = AppSpec {
        instance: format!("{}-{}", instance(home), app.name),
        image,
        env,
        data_dir: files.data_dir,
    };
    runner.start(&spec).map(Some)
}

/// Start the apps behind `app`'s connector. What it started is stopped again if one fails.
fn start_apps(home: &Path, app: &ToonApp, runner: &dyn AppRunner) -> Result<StartedApps, Error> {
    let mut started = StartedApps::new();
    for behind in &app.apps {
        match start_app(home, app, behind, runner) {
            Ok(Some(running)) => started.push((behind.name.clone(), running)),
            Ok(None) => {}
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
    runner: Box<dyn AppRunner>,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    // A hidden service is not started without its overlay, and never on clearnet instead,
    // so the overlay comes up before anything behind it listens.
    let overlay = match app.reach {
        Reach::Hidden => {
            let edge = overlay::bootstrap(home)?;
            let key = ConnectorFiles::of(home, app.connector).onion_key;
            let endpoint = edge.issue(app.connector, &key)?;
            Some((edge, endpoint))
        }
        Reach::Clearnet { .. } => None,
    };
    let apps = start_apps(home, app, &*runner)?;
    launch_connector(home, app, overlay, apps, runner, listener, socket)
}

/// Where each running app's write port is reached.
fn addresses(apps: &StartedApps) -> Vec<(String, SocketAddr)> {
    apps.iter()
        .map(|(name, running)| (name.clone(), running.write_address()))
        .collect()
}

fn statuses(apps: &StartedApps) -> Vec<AppStatus> {
    apps.iter()
        .map(|(name, running)| AppStatus {
            name: name.clone(),
            address: running.write_address(),
            running: AtomicBool::new(true),
        })
        .collect()
}

fn launch_connector(
    home: &Path,
    app: &ToonApp,
    overlay: Option<(Box<dyn Edge>, String)>,
    mut apps: StartedApps,
    runner: Box<dyn AppRunner>,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    let hidden = overlay.map(|(edge, endpoint)| Hidden {
        edge,
        endpoint,
        read: read_address(&apps),
    });
    let rendering = hidden.as_ref().map(Hidden::overlay);
    let started =
        node::render(home, app, &addresses(&apps), rendering.as_ref()).and_then(|files| {
            let started = spawn(&files)?;
            Ok((files, started))
        });
    let (files, started) = match started {
        Ok(started) => started,
        Err(error) => {
            for (_, running) in &mut apps {
                running.stop();
            }
            return Err(error);
        }
    };
    if let Some(hidden) = &hidden {
        hidden.publish(&started.address);
    }
    let first = (started.child.id(), started.address.clone());
    let shared = Arc::new(Shared {
        toon_app: app.name.clone(),
        apps: Mutex::new(statuses(&apps)),
        reload: Mutex::new(Vec::new()),
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
    thread::spawn(move || control::serve(listener, move |request| answering.answer(request)));
    Ok(Supervisor {
        hidden,
        connector: Some(started),
        apps,
        runner,
        app: app.clone(),
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
                        "apps": self.apps().iter().map(|app| json!({
                            "name": app.name,
                            "address": app.address.to_string(),
                            "running": app.running.load(Ordering::SeqCst),
                        })).collect::<Vec<_>>(),
                    }],
                })
            }
            "reload" => self.reload(),
            "down" => {
                self.stop.store(true, Ordering::SeqCst);
                json!({ "stopping": true })
            }
            _ => json!({ "error": "unknown request" }),
        }
    }

    /// Ask `wait` to start the connector again from the state as it now is, and wait for
    /// the answer. Apps are started and stopped to match the state too.
    fn reload(&self) -> Value {
        let (answer, answered) = mpsc::channel();
        self.reloads().push(answer);
        match answered.recv() {
            Ok(Ok(())) => json!({ "reloaded": true }),
            Ok(Err(Unreloaded { error, stopped })) => {
                let mut reply = error.json();
                reply["stopped"] = json!(stopped);
                reply
            }
            Err(_) => json!({ "error": { "code": "connector_failed",
                "message": "The supervisor stopped before it reloaded." } }),
        }
    }

    fn reloads(&self) -> std::sync::MutexGuard<'_, Vec<Reload>> {
        self.reload.lock().expect("the reload requests")
    }

    fn apps(&self) -> std::sync::MutexGuard<'_, Vec<AppStatus>> {
        self.apps.lock().expect("the apps' state")
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
            // Requests that came in together are carried out by one restart.
            let requested = std::mem::take(&mut *self.shared.reloads());
            if !requested.is_empty() {
                let reloaded = self.reload();
                for answer in requested {
                    let _ = answer.send(match &reloaded {
                        Ok(()) => Ok(()),
                        Err(unreloaded) => Err(Unreloaded {
                            error: Error {
                                code: unreloaded.error.code,
                                message: unreloaded.error.message.clone(),
                            },
                            stopped: unreloaded.stopped,
                        }),
                    });
                }
                continue;
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
        // Whoever still waits for a reload is told the supervisor stopped.
        self.shared.reloads().clear();
        self.stop_connector();
        // The apps go once the connector that delivers to them has.
        for (_, app) in &mut self.apps {
            app.stop();
        }
        self.hidden = None;
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
            // A reload starts the connector itself, without waiting out the delay.
            if self.shared.stop.load(Ordering::SeqCst) || !self.shared.reloads().is_empty() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.delay = (self.delay * 2).min(LONGEST_RESTART_DELAY);
        match spawn(&self.files) {
            Ok(started) => {
                if let Some(hidden) = &self.hidden {
                    hidden.publish(&started.address);
                }
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

    /// Start the connector again with the apps and routes the state now holds. An app that
    /// is new starts first, and the new config is checked, so that a change that fails
    /// either leaves the connector as it was. Then the connector stops, which drops what it
    /// holds in flight, and the apps that were removed stop after it.
    fn reload(&mut self) -> Result<(), Unreloaded> {
        let untouched = |error| Unreloaded {
            error,
            stopped: false,
        };
        let state = State::load(&self.home)
            .map_err(untouched)?
            .ok_or_else(|| untouched(node::no_agent_node(&self.home)))?;
        let toon_app = self.shared.toon_app.clone();
        let Some(app) = state.toon_apps.iter().find(|app| app.name == toon_app) else {
            return Err(untouched(failed(format!(
                "The TOON app {toon_app} is no longer in the state."
            ))));
        };
        // An app whose definition changed, or the relay with new settings, is started again,
        // and the old one goes first: the two would share a name.
        let mut touched = false;
        for behind in &app.apps {
            let old = self.app.apps.iter().find(|old| old.name == behind.name);
            let changed = old.is_some_and(|old| {
                old.source != behind.source
                    || (behind.name == node::RELAY && self.app.relay != app.relay)
            });
            if let Some(position) = self.apps.iter().position(|(name, _)| *name == behind.name) {
                if changed {
                    self.apps.remove(position).1.stop();
                    touched = true;
                }
            }
        }
        let unreloaded = |error| Unreloaded {
            error,
            stopped: touched,
        };
        let mut fresh = StartedApps::new();
        let stop = |fresh: &mut StartedApps| {
            for (_, running) in fresh {
                running.stop();
            }
        };
        for behind in &app.apps {
            if self.apps.iter().any(|(name, _)| *name == behind.name) {
                continue;
            }
            match start_app(&self.home, app, behind, &*self.runner) {
                Ok(Some(running)) => fresh.push((behind.name.clone(), running)),
                Ok(None) => {}
                Err(error) => {
                    stop(&mut fresh);
                    return Err(unreloaded(error));
                }
            }
        }
        let mut reached = addresses(&self.apps);
        reached.extend(addresses(&fresh));
        // The relay's read port moves with the relay, so the onion endpoint is told again.
        let read = read_address(&self.apps).or_else(|| read_address(&fresh));
        let rendering = self.hidden.as_ref().map(Hidden::overlay);
        let files = match node::render(&self.home, app, &reached, rendering.as_ref()) {
            Ok(files) => files,
            Err(error) => {
                stop(&mut fresh);
                // Put back the config the connector runs, for the next time it restarts.
                let _ = node::render(
                    &self.home,
                    &self.app,
                    &addresses(&self.apps),
                    rendering.as_ref(),
                );
                return Err(unreloaded(error));
            }
        };
        self.stop_connector();
        {
            let mut live = self.shared.live();
            live.running = false;
            live.pid = None;
            live.address = None;
        }
        let (kept, removed): (StartedApps, StartedApps) = std::mem::take(&mut self.apps)
            .into_iter()
            .partition(|(name, _)| app.apps.iter().any(|behind| behind.name == *name));
        for (_, mut running) in removed {
            running.stop();
        }
        self.apps = kept;
        self.apps.extend(fresh);
        *self.shared.apps() = statuses(&self.apps);
        self.app = app.clone();
        if let Some(hidden) = &mut self.hidden {
            hidden.read = read;
        }
        self.files = files;
        let started = spawn(&self.files).map_err(|error| Unreloaded {
            error,
            stopped: true,
        })?;
        if let Some(hidden) = &self.hidden {
            hidden.publish(&started.address);
        }
        let mut live = self.shared.live();
        live.pid = Some(started.child.id());
        live.address = Some(started.address.clone());
        live.running = true;
        drop(live);
        self.connector = Some(started);
        self.delay = FIRST_RESTART_DELAY;
        Ok(())
    }

    /// The name of an app that has stopped, if one has. Each app's status is kept current.
    fn stopped_app(&mut self) -> Option<String> {
        let mut stopped = None;
        for ((_, running), status) in self.apps.iter_mut().zip(self.shared.apps().iter()) {
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
