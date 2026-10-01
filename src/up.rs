//! The supervisor, in the foreground: what `toon up --foreground` runs, and what the
//! `systemd --user` unit (`service`) runs.
//!
//! It renders each TOON app's connector config from the agent node's state, starts the
//! connector as a child process of this same binary (`connector`), restarts it when it
//! exits, and serves lifecycle requests on a local socket (`control`): `status` asks what
//! runs, and `down` stops the connector and then this process.

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::connector::{self, Startup};
use crate::control;
use crate::node::{self, ConnectorFiles, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long a connector gets to exit once its supervisor is stopping, before it is killed.
const GRACE: Duration = Duration::from_secs(10);

/// How long the supervisor waits before it restarts a connector that has just stopped,
/// and the longest it waits between attempts that keep failing.
const FIRST_RESTART_DELAY: Duration = Duration::from_millis(250);
const LONGEST_RESTART_DELAY: Duration = Duration::from_secs(30);

/// A connector that ran this long was healthy, so its next restart starts over at the
/// shortest delay.
const HEALTHY: Duration = Duration::from_secs(60);

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
    files: ConnectorFiles,
    shared: Arc<Shared>,
    socket: PathBuf,
    home: PathBuf,
    /// What the first start reported, for `report`.
    first: (u32, String),
    /// How long to wait before the next restart.
    delay: Duration,
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
    match launch(home, app, listener, &socket) {
        Ok(supervisor) => Ok(supervisor),
        Err(error) => {
            let _ = std::fs::remove_file(&socket);
            Err(error)
        }
    }
}

fn launch(
    home: &Path,
    app: &ToonApp,
    listener: UnixListener,
    socket: &Path,
) -> Result<Supervisor, Error> {
    let files = node::render(home, app)?;
    let started = spawn(&files)?;
    let first = (started.child.id(), started.address.clone());
    let shared = Arc::new(Shared {
        toon_app: app.name.clone(),
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

    /// Stay in the foreground until `toon down`. A connector that stops by itself is
    /// started again, after a delay that doubles while the starts keep failing.
    pub fn wait(mut self) {
        while !self.shared.stop.load(Ordering::SeqCst) {
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
        self.shared.live().running = false;
        // Last, so that a `toon down` that sees the socket gone knows everything has.
        let _ = std::fs::remove_file(&self.socket);
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
