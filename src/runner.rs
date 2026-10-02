//! App runners: how an app behind a connector is started and stopped.
//!
//! An app is a payment-oblivious HTTP service, so all a runner promises is a running
//! process that answers `GET /health` on a write address only this machine can reach, and
//! that stops when asked. The real runner starts a container from an image. The other
//! starts a local process, and is what the tests use: `TOON_APP_COMMAND` names the
//! program, and it is used for every app in place of its image. Both are held to one
//! contract suite, in the tests below.

use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::outcome::{Error, ErrorCode};

/// The environment variable that swaps the container runner for a local process.
pub const COMMAND_VARIABLE: &str = "TOON_APP_COMMAND";

/// The port an app serves writes on inside its container, and the one a local process is
/// told to use through `TOON_BLS_PORT`.
pub const WRITE_PORT: u16 = 3100;

/// The port the relay serves reads on inside its container, and the one a local process
/// is told to use through `TOON_RELAY_PORT`, the name the relay's image reads.
pub const READ_PORT: u16 = crate::overlay::RELAY_READ_PORT;

/// How long an app gets to answer its health check once started.
const HEALTHY_WITHIN: Duration = Duration::from_secs(120);

/// How long an app gets to exit once asked to stop, before it is killed.
const GRACE: Duration = Duration::from_secs(10);

/// What a runner is asked to start.
#[derive(Clone, Debug)]
pub struct AppSpec {
    /// Names the app on this machine: unique per agent node, so a runner can find an
    /// app a dead supervisor left behind.
    pub instance: String,
    /// The image the container runner starts.
    pub image: String,
    /// The environment the app is started with. It may carry secrets, so a runner does
    /// not put it on a command line.
    pub env: Vec<(String, String)>,
    /// Where the app keeps what it must not lose.
    pub data_dir: PathBuf,
    /// Whether the app is the relay. A container runner starts the relay on the host's
    /// network, both listeners on loopback ports it picks, so that it can ask a connector
    /// that listens on loopback (ADR 0006). Every other app stays on docker's bridge.
    pub relay: bool,
}

pub trait AppRunner {
    /// Start the app and return once it answers its health check.
    fn start(&self, spec: &AppSpec) -> Result<Box<dyn RunningApp>, Error>;
}

pub trait RunningApp: Send {
    /// Where the app's write port can be reached: this machine only.
    fn write_address(&self) -> SocketAddr;
    /// Where the app's read port can be reached, if it has one: this machine only.
    fn read_address(&self) -> Option<SocketAddr>;
    /// Whether the app is still running.
    fn running(&mut self) -> bool;
    /// Stop the app and return once it is gone. Stopping a stopped app does nothing.
    fn stop(&mut self);
}

fn failed(message: String) -> Error {
    Error {
        code: ErrorCode::AppFailed,
        message,
    }
}

/// The runner this machine uses: a local process if `TOON_APP_COMMAND` is set, else a
/// container.
pub fn from_environment() -> Box<dyn AppRunner> {
    match env::var_os(COMMAND_VARIABLE).filter(|command| !command.is_empty()) {
        Some(command) => Box::new(ProcessRunner {
            program: PathBuf::from(command),
        }),
        None => Box::new(ContainerRunner),
    }
}

/// A loopback port that is free until something else takes it; the app fails to bind and
/// says so.
fn free_port() -> Result<u16, Error> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| failed(format!("No free port for the app: {error}.")))
}

/// Wait for `GET /health` on `address` to answer 200. `alive` says whether the app is
/// still worth waiting for.
fn wait_healthy(address: SocketAddr, mut alive: impl FnMut() -> bool) -> Result<(), String> {
    let deadline = Instant::now() + HEALTHY_WITHIN;
    loop {
        if healthy(address) {
            return Ok(());
        }
        if !alive() {
            return Err("it stopped before it answered its health check".into());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "it did not answer its health check within {} seconds",
                HEALTHY_WITHIN.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn healthy(address: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let request = "GET /health HTTP/1.1\r\nHost: app\r\nConnection: close\r\n\r\n";
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer.starts_with("HTTP/1.1 200")
}

/// Runs an app as a child process of the supervisor, on a loopback port it picks.
pub struct ProcessRunner {
    pub program: PathBuf,
}

struct Process {
    child: Child,
    /// The app exits when this closes, so it is held for as long as the app runs.
    alive: Option<ChildStdin>,
    address: SocketAddr,
    read: SocketAddr,
}

impl AppRunner for ProcessRunner {
    fn start(&self, spec: &AppSpec) -> Result<Box<dyn RunningApp>, Error> {
        let io =
            |path: &Path, error: std::io::Error| failed(format!("{}: {error}.", path.display()));
        fs::create_dir_all(&spec.data_dir).map_err(|error| io(&spec.data_dir, error))?;
        let log = spec.data_dir.join("app.log");
        let logs = File::options()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|error| io(&log, error))?;
        let (port, read_port) = (free_port()?, free_port()?);
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .env("TOON_BLS_PORT", port.to_string())
            .env("TOON_RELAY_PORT", read_port.to_string())
            .env("TOON_DATA_DIR", &spec.data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(
                logs.try_clone().map_err(|error| io(&log, error))?,
            ))
            .stderr(logs);
        let mut child = command.spawn().map_err(|error| {
            failed(format!(
                "{} could not be started: {error}.",
                self.program.display()
            ))
        })?;
        let alive = child.stdin.take();
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut process = Process {
            child,
            alive,
            address,
            read: SocketAddr::from((Ipv4Addr::LOCALHOST, read_port)),
        };
        if let Err(why) = wait_healthy(address, || process.running()) {
            process.stop();
            return Err(failed(format!(
                "The app {} ({}): {why}. Its log is {}.",
                spec.instance,
                self.program.display(),
                log.display()
            )));
        }
        Ok(Box::new(process))
    }
}

impl RunningApp for Process {
    fn write_address(&self) -> SocketAddr {
        self.address
    }

    fn read_address(&self) -> Option<SocketAddr> {
        Some(self.read)
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        drop(self.alive.take());
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs an app as a container of `docker`, its write port published on loopback only. The
/// relay is the exception: it shares the host's network and binds loopback.
pub struct ContainerRunner;

struct Container {
    name: String,
    address: SocketAddr,
    read: SocketAddr,
    stopped: bool,
}

fn docker(args: &[&str]) -> Result<String, String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .map_err(|error| format!("docker could not be run: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

impl AppRunner for ContainerRunner {
    fn start(&self, spec: &AppSpec) -> Result<Box<dyn RunningApp>, Error> {
        let name = format!("toon-{}", spec.instance);
        fs::create_dir_all(&spec.data_dir)
            .map_err(|error| failed(format!("{}: {error}.", spec.data_dir.display())))?;
        let data = fs::canonicalize(&spec.data_dir)
            .map_err(|error| failed(format!("{}: {error}.", spec.data_dir.display())))?;
        // What a supervisor that was killed left behind.
        let _ = docker(&["rm", "--force", &name]);

        let mut command = Command::new("docker");
        command.args(["run", "--detach", "--rm", "--name", &name]);
        // The relay is told its ports; any other app is told nothing but where it is
        // published.
        let relay_ports = if spec.relay {
            let (write, read) = (free_port()?, free_port()?);
            command
                .args(["--network", "host"])
                .args(["--env", "TOON_HOST=127.0.0.1"])
                .args(["--env", "TOON_WRITE_HOST=127.0.0.1"])
                .args(["--env", &format!("TOON_BLS_PORT={write}")])
                .args(["--env", &format!("TOON_RELAY_PORT={read}")]);
            Some((write, read))
        } else {
            command
                .args(["--publish", &format!("127.0.0.1::{WRITE_PORT}")])
                .args(["--publish", &format!("127.0.0.1::{READ_PORT}")])
                .args(["--env", &format!("TOON_BLS_PORT={WRITE_PORT}")]);
            None
        };
        command
            .arg("--volume")
            .arg(format!("{}:/data", data.display()))
            .args(["--env", "TOON_DATA_DIR=/data"]);
        // `--env NAME` takes the value from this process's environment, so a secret is
        // never in the argument list that `ps` shows.
        for (variable, value) in &spec.env {
            command.args(["--env", variable]).env(variable, value);
        }
        let output = command
            .arg(&spec.image)
            .output()
            .map_err(|error| failed(format!("docker could not be run: {error}.")))?;
        if !output.status.success() {
            return Err(failed(format!(
                "docker could not start {}: {}",
                spec.image,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let mut container = Container {
            name: name.clone(),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            read: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            stopped: false,
        };
        let published = |port: u16| {
            docker(&["port", &name, &format!("{port}/tcp")]).and_then(|ports| {
                ports
                    .lines()
                    .filter_map(|line| line.parse::<SocketAddr>().ok())
                    .find(SocketAddr::is_ipv4)
                    .ok_or_else(|| format!("docker did not say where {name} publishes {port}"))
            })
        };
        let reached = match relay_ports {
            Some((write, read)) => Ok((
                SocketAddr::from((Ipv4Addr::LOCALHOST, write)),
                SocketAddr::from((Ipv4Addr::LOCALHOST, read)),
            )),
            None => published(WRITE_PORT).and_then(|write| Ok((write, published(READ_PORT)?))),
        };
        match reached {
            Ok((write, read)) => {
                container.address = write;
                container.read = read;
            }
            Err(why) => {
                container.stop();
                return Err(failed(format!("The app {}: {why}.", spec.instance)));
            }
        }
        let address = container.address;
        if let Err(why) = wait_healthy(address, || container.running()) {
            let logs = docker(&["logs", "--tail", "20", &name]).unwrap_or_default();
            container.stop();
            return Err(failed(format!(
                "The app {} ({}): {why}. {logs}",
                spec.instance, spec.image
            )));
        }
        Ok(Box::new(container))
    }
}

impl RunningApp for Container {
    fn write_address(&self) -> SocketAddr {
        self.address
    }

    fn read_address(&self) -> Option<SocketAddr> {
        Some(self.read)
    }

    fn running(&mut self) -> bool {
        docker(&["inspect", "--format", "{{.State.Running}}", &self.name])
            .is_ok_and(|running| running == "true")
    }

    fn stop(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        let _ = docker(&["stop", "--time", "10", &self.name]);
        // `--rm` removes it once stopped; this is for one that was created and never ran.
        let _ = docker(&["rm", "--force", &self.name]);
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake relay of `examples/fake_relay.rs`, built alongside the tests.
    fn fake_relay() -> PathBuf {
        let tests = env::current_exe().expect("the test binary's path");
        let program = tests
            .parent()
            .and_then(Path::parent)
            .expect("target/<profile>/deps/<test>")
            .join("examples")
            .join("fake_relay");
        assert!(program.exists(), "{} is not built", program.display());
        program
    }

    fn spec(data_dir: &Path, image: &str) -> AppSpec {
        AppSpec {
            instance: format!("contract-{}", std::process::id()),
            image: image.into(),
            env: vec![("NOSTR_SECRET_KEY".into(), "02".repeat(32))],
            data_dir: data_dir.to_path_buf(),
            relay: false,
        }
    }

    fn get(address: SocketAddr, path: &str) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1)).ok()?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: app\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .ok()?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer).ok()?;
        Some(answer)
    }

    /// What every runner promises, held against the runner and the app it starts.
    fn contract(runner: &dyn AppRunner, spec: &AppSpec) {
        let mut app = runner.start(spec).expect("the app starts");
        let address = app.write_address();
        assert!(
            address.ip().is_loopback(),
            "the write port is on loopback: {address}"
        );
        assert!(app.running());
        assert!(
            get(address, "/health").is_some_and(|answer| answer.starts_with("HTTP/1.1 200")),
            "a started app answers its health check"
        );

        app.stop();

        assert!(!app.running(), "a stopped app is not running");
        assert!(
            get(address, "/health").is_none(),
            "a stopped app answers nothing"
        );
        app.stop();
    }

    #[test]
    fn the_local_process_runner_meets_the_contract() {
        let data = tempfile::tempdir().unwrap();
        let runner = ProcessRunner {
            program: fake_relay(),
        };
        contract(&runner, &spec(data.path(), "unused"));
    }

    #[test]
    fn a_local_process_is_given_its_environment_and_nothing_else() {
        let data = tempfile::tempdir().unwrap();
        let runner = ProcessRunner {
            program: fake_relay(),
        };
        let mut app = runner.start(&spec(data.path(), "unused")).unwrap();

        assert_eq!(
            fs::read_to_string(data.path().join("environment")).unwrap(),
            format!("NOSTR_SECRET_KEY={}\n", "02".repeat(32))
        );
        app.stop();
    }

    #[test]
    fn a_local_process_that_exits_is_reported_as_not_running() {
        let data = tempfile::tempdir().unwrap();
        let runner = ProcessRunner {
            program: fake_relay(),
        };
        let mut starting = spec(data.path(), "unused");
        starting
            .env
            .push(("FAKE_RELAY_EXIT_AFTER_START".into(), "1".into()));
        // Either it never answers, or it answers and is found gone: not running either way.
        match runner.start(&starting) {
            Ok(mut app) => {
                let deadline = Instant::now() + Duration::from_secs(10);
                while app.running() {
                    assert!(Instant::now() < deadline, "the app is still running");
                    thread::sleep(Duration::from_millis(20));
                }
            }
            Err(error) => assert_eq!(error.code, ErrorCode::AppFailed),
        }
    }

    #[test]
    fn a_program_that_is_not_there_is_an_app_failure() {
        let data = tempfile::tempdir().unwrap();
        let runner = ProcessRunner {
            program: data.path().join("no-such-program"),
        };

        let error = runner.start(&spec(data.path(), "unused")).err().unwrap();

        assert_eq!(error.code, ErrorCode::AppFailed);
    }

    /// The same contract, against the real relay image. It needs a docker daemon that
    /// can pull the image, so it runs on request: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs docker and a network to pull the relay image"]
    fn the_container_runner_meets_the_contract() {
        let data = tempfile::tempdir().unwrap();
        let mut relay = spec(data.path(), env!("TOON_RELAY_IMAGE"));
        relay.relay = true;
        contract(&ContainerRunner, &relay);

        // The relay is on the host's network: both addresses are on loopback, and the
        // health check answers there.
        let mut app = ContainerRunner.start(&relay).expect("the relay starts");
        let read = app.read_address().expect("the relay has a read address");
        assert!(
            read.ip().is_loopback(),
            "the read port is on loopback: {read}"
        );
        assert!(app.write_address().ip().is_loopback());
        assert_ne!(read.port(), app.write_address().port());
        assert!(get(app.write_address(), "/health")
            .is_some_and(|answer| answer.starts_with("HTTP/1.1 200")));
        app.stop();
    }
}
