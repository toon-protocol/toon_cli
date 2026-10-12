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
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::outcome::{Error, ErrorCode};
use crate::ports::{self, Passing};

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

/// The default limit on an app's memory, in bytes (ADR 0011).
pub const DEFAULT_MEMORY: u64 = 512 * 1024 * 1024;
/// The default limit on an app's CPUs.
pub const DEFAULT_CPUS: f64 = 1.0;
/// The default limit on an app's processes.
pub const DEFAULT_PIDS: u64 = 256;
/// The size of the tmpfs every container gets at `/tmp`.
const TMP_SIZE: &str = "64m";

/// The fixed sentence an `app_failed` message carries when a container did not start or did
/// not answer its health check.
const CONFINED: &str = "The app runs confined: see \"What an app may and may not do\" in \
     docs/guide/adding-an-app.md for the allowances.";

/// What an operator allowed one app when adding it (ADR 0011). Kept with the app in the
/// home. The default allows nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Allowances {
    /// The app may open connections out.
    pub egress: bool,
    /// Further writable tmpfs paths, emptied on every restart.
    pub scratch: Vec<String>,
    /// A memory limit in bytes, in place of the default.
    pub memory: Option<u64>,
    /// A CPU limit, in place of the default.
    pub cpus: Option<f64>,
    /// A limit on processes, in place of the default.
    pub pids: Option<u64>,
}

/// `cpus` is never NaN: `parse` refuses it.
impl Eq for Allowances {}

/// What `toon change` does to one limit.
#[derive(Clone, Copy, Debug, Default)]
pub enum Limit<T> {
    /// The flag was not given.
    #[default]
    Keep,
    /// `default`: the limit goes back to the default.
    Default,
    Set(T),
}

impl<T> Limit<T> {
    fn over(self, before: Option<T>) -> Option<T> {
        match self {
            Limit::Keep => before,
            Limit::Default => None,
            Limit::Set(value) => Some(value),
        }
    }

    fn given(&self) -> bool {
        !matches!(self, Limit::Keep)
    }
}

/// What the allowance flags of `toon change` ask for: a patch over an app's allowances.
#[derive(Clone, Debug, Default)]
pub struct AllowanceChange {
    /// `Some(true)` for `--allow-egress`, `Some(false)` for `--no-allow-egress`.
    pub egress: Option<bool>,
    pub scratch: Vec<String>,
    pub unset_scratch: Vec<String>,
    pub memory: Limit<u64>,
    pub cpus: Limit<f64>,
    pub pids: Limit<u64>,
}

impl AllowanceChange {
    /// Whether no allowance flag was given.
    pub fn is_empty(&self) -> bool {
        self.egress.is_none()
            && self.scratch.is_empty()
            && self.unset_scratch.is_empty()
            && !self.memory.given()
            && !self.cpus.given()
            && !self.pids.given()
    }

    /// Whether a flag other than a limit was given.
    pub fn more_than_limits(&self) -> bool {
        self.egress.is_some() || !self.scratch.is_empty() || !self.unset_scratch.is_empty()
    }
}

fn usage(message: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::Usage,
        message,
    }
}

/// A size such as `512m`, `2g`, `64k` or a plain number of bytes, in bytes.
fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_suffix('b').unwrap_or(&text);
    let (digits, unit) = match text.chars().last()? {
        'k' => (&text[..text.len() - 1], 1u64 << 10),
        'm' => (&text[..text.len() - 1], 1 << 20),
        'g' => (&text[..text.len() - 1], 1 << 30),
        _ => (text, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(unit))
        .filter(|bytes| *bytes > 0)
}

impl Allowances {
    /// The allowances the flags of `toon add` and `toon create` ask for, or `usage` for a
    /// value that cannot be used.
    pub fn parse(
        egress: bool,
        scratch: &[String],
        memory: Option<&str>,
        cpus: Option<&str>,
        pids: Option<&str>,
    ) -> Result<Self, Error> {
        let mut paths: Vec<String> = Vec::new();
        for path in scratch {
            let bare = path.trim_end_matches('/');
            let bare = if bare.is_empty() { "/" } else { bare };
            let refused = |why: &str| usage(format!("--scratch {path}: {why}."));
            if !path.starts_with('/') {
                return Err(refused("it must be an absolute path in the container"));
            }
            if matches!(bare, "/" | "/data" | "/tmp") {
                return Err(refused("`/`, `/data` and `/tmp` are already as they are"));
            }
            if path.contains([':', ',', '\n']) || bare.split('/').any(|part| part == "..") {
                return Err(refused("it must not hold `:`, `,`, a newline or `..`"));
            }
            if !paths.iter().any(|known| known == bare) {
                paths.push(bare.to_owned());
            }
        }
        let memory = memory
            .map(|text| {
                parse_size(text).ok_or_else(|| {
                    usage(format!(
                        "--memory {text}: a size such as 512m or 2g, above zero."
                    ))
                })
            })
            .transpose()?;
        let cpus = cpus
            .map(|text| {
                text.parse::<f64>()
                    .ok()
                    .filter(|cpus| cpus.is_finite() && *cpus > 0.0)
                    .ok_or_else(|| usage(format!("--cpus {text}: a number above zero.")))
            })
            .transpose()?;
        let pids = pids
            .map(|text| {
                text.parse::<u64>()
                    .ok()
                    .filter(|pids| *pids > 0)
                    .ok_or_else(|| usage(format!("--pids {text}: a whole number above zero.")))
            })
            .transpose()?;
        Ok(Self {
            egress,
            scratch: paths,
            memory,
            cpus,
            pids,
        })
    }

    /// The patch the flags of `toon change` ask for, with the values checked as `parse`
    /// checks them. A limit given as `default` goes back to the default.
    pub fn parse_change(
        egress: Option<bool>,
        scratch: &[String],
        unset_scratch: &[String],
        memory: Option<&str>,
        cpus: Option<&str>,
        pids: Option<&str>,
    ) -> Result<AllowanceChange, Error> {
        fn value(text: Option<&str>) -> Option<&str> {
            text.filter(|text| *text != "default")
        }
        let set = Self::parse(false, scratch, value(memory), value(cpus), value(pids))?;
        // The same spellings of a path, whether it is added or taken away.
        let unset = Self::parse(false, unset_scratch, None, None, None)?;
        fn limit<T>(text: Option<&str>, parsed: Option<T>) -> Limit<T> {
            match (text, parsed) {
                (None, _) => Limit::Keep,
                (Some(_), Some(value)) => Limit::Set(value),
                (Some(_), None) => Limit::Default,
            }
        }
        Ok(AllowanceChange {
            egress,
            scratch: set.scratch,
            unset_scratch: unset.scratch,
            memory: limit(memory, set.memory),
            cpus: limit(cpus, set.cpus),
            pids: limit(pids, set.pids),
        })
    }

    /// This allowance with `change` applied over it, or `usage` if the change takes away
    /// something the app does not have.
    pub fn changed(&self, change: &AllowanceChange) -> Result<Self, Error> {
        let mut result = self.clone();
        if change.egress == Some(true) {
            result.egress = true;
        }
        if change.egress == Some(false) {
            if !self.egress {
                return Err(usage(
                    "--no-allow-egress: the app is not allowed to open connections out.".into(),
                ));
            }
            result.egress = false;
        }
        for path in &change.unset_scratch {
            if change.scratch.contains(path) {
                return Err(usage(format!("--scratch {path}: the path is given twice.")));
            }
            let Some(position) = result.scratch.iter().position(|known| known == path) else {
                let has = if self.scratch.is_empty() {
                    "none".to_owned()
                } else {
                    self.scratch.join(", ")
                };
                return Err(usage(format!(
                    "--unset-scratch {path}: the app has no such scratch path. Its scratch \
                     paths: {has}."
                )));
            };
            result.scratch.remove(position);
        }
        for path in &change.scratch {
            if !result.scratch.contains(path) {
                result.scratch.push(path.clone());
            }
        }
        result.memory = change.memory.over(result.memory);
        result.cpus = change.cpus.over(result.cpus);
        result.pids = change.pids.over(result.pids);
        Ok(result)
    }

    /// Whether nothing is allowed.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn memory(&self) -> u64 {
        self.memory.unwrap_or(DEFAULT_MEMORY)
    }

    fn cpus(&self) -> f64 {
        self.cpus.unwrap_or(DEFAULT_CPUS)
    }

    fn pids(&self) -> u64 {
        self.pids.unwrap_or(DEFAULT_PIDS)
    }

    /// How `toon status --json` says an app is confined: what the supervisor starts it
    /// with, derived from the allowances and not read back from docker. `user` is the
    /// `uid:gid` of the process that runs `toon`.
    pub fn confinement(&self, relay: bool, user: &str) -> serde_json::Value {
        let mut writable = vec!["/data".to_owned(), "/tmp".to_owned()];
        writable.extend(self.scratch.iter().cloned());
        serde_json::json!({
            "network": if relay { "host" } else if self.egress { "egress" } else { "none" },
            "user": user,
            "read_only": true,
            "writable": writable,
            "capabilities": [],
            "memory": self.memory(),
            "cpus": self.cpus(),
            "pids": self.pids(),
        })
    }

    /// A sentence for plain `toon status` where the app departs from the default.
    pub fn departures(&self) -> Option<String> {
        if self.is_default() {
            return None;
        }
        let mut parts = Vec::new();
        if self.egress {
            parts.push("may open connections out".to_owned());
        }
        if !self.scratch.is_empty() {
            parts.push(format!("has scratch {}", self.scratch.join(", ")));
        }
        if let Some(memory) = self.memory {
            parts.push(format!("memory {memory} bytes"));
        }
        if let Some(cpus) = self.cpus {
            parts.push(format!("{cpus} CPUs"));
        }
        if let Some(pids) = self.pids {
            parts.push(format!("{pids} processes"));
        }
        Some(parts.join(", "))
    }
}

/// The `uid:gid` of the process that runs `toon`: the user a container runs as.
pub fn caller() -> String {
    use std::os::unix::fs::MetadataExt;
    // The owner of `/proc/self` is the process's own user and group.
    if let Ok(own) = fs::metadata("/proc/self") {
        return format!("{}:{}", own.uid(), own.gid());
    }
    let id = |flag: &str| {
        Command::new("id")
            .arg(flag)
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    format!("{}:{}", id("-u"), id("-g"))
}

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
    /// Files of the machine the app finds read-only in its container: the copy and the
    /// path. A runner with no container ignores them.
    pub mounts: Vec<(PathBuf, String)>,
    /// Where the app keeps what it must not lose.
    pub data_dir: PathBuf,
    /// Whether the app is the relay. A container runner starts the relay on the host's
    /// network, both listeners on loopback ports it picks, so that it can ask a connector
    /// that listens on loopback (ADR 0006). Every other app has a network of its own (ADR 0011).
    pub relay: bool,
    /// What the operator allowed the app. The relay always has the defaults.
    pub allowances: Allowances,
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
        nothing_sent: false,
        unanswered: None,
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

/// Two loopback ports, a write port and a read port. They are free now and the app binds
/// them later, so they are ports the system does not hand out by itself, and they are
/// claimed for this app until what is returned is dropped (`ports::passing`), so they differ
/// and no other supervisor gives them to an app of its own. If another program takes one
/// all the same, the app fails to bind and says so.
fn free_ports() -> Result<(Passing, Passing), Error> {
    let free = || {
        ports::passing(&Ipv4Addr::LOCALHOST.to_string())
            .map_err(|error| failed(format!("No free port for the app: {error}.")))
    };
    let write = free()?;
    loop {
        let read = free()?;
        // Only ports the system was left to pick, which are not claimed, can be the same.
        if read.port != write.port {
            return Ok((write, read));
        }
    }
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
        let log = app_log(&spec.data_dir);
        let logs = File::options()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|error| io(&log, error))?;
        // Held until the app answers, which it does once it has bound them.
        let claimed = free_ports()?;
        let (port, read_port) = (claimed.0.port, claimed.1.port);
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

/// Runs an app as a container of `docker`, confined (ADR 0011), on a network of its own that
/// publishes no port. The relay is the exception: it shares the host's network and binds
/// loopback.
pub struct ContainerRunner;

struct Container {
    name: String,
    /// The docker network made for the app, if it is not the relay.
    network: Option<String>,
    address: SocketAddr,
    read: SocketAddr,
    stopped: bool,
    /// Where the container's output is kept once it is gone.
    log: PathBuf,
}

/// The file an app's own output is kept in, in its data directory.
pub fn app_log(data_dir: &Path) -> PathBuf {
    data_dir.join("app.log")
}

/// Append what the container wrote to its standard output and standard error to `log`, in
/// the order it wrote them, so that it outlives the container. A container that is not there
/// keeps nothing.
fn keep_output(name: &str, log: &Path) {
    // Else `docker logs` would write its own complaint into the app's log.
    if docker(&["container", "inspect", name]).is_err() {
        return;
    }
    let Ok(file) = File::options().create(true).append(true).open(log) else {
        return;
    };
    let Ok(errors) = file.try_clone() else {
        return;
    };
    let _ = Command::new("docker")
        .args(["logs", name])
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(errors)
        .status();
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

/// How `docker run` places an app on the network. The relay, given its write and read ports,
/// shares the host's network and is told to bind both on loopback; any other app joins the
/// docker network made for it, publishes nothing, and is told only its write port.
fn network_arguments(relay_ports: Option<(u16, u16)>, network: &str) -> Vec<String> {
    match relay_ports {
        Some((write, read)) => vec![
            "--network".to_owned(),
            "host".to_owned(),
            "--env".to_owned(),
            "TOON_HOST=127.0.0.1".to_owned(),
            "--env".to_owned(),
            "TOON_WRITE_HOST=127.0.0.1".to_owned(),
            "--env".to_owned(),
            format!("TOON_BLS_PORT={write}"),
            "--env".to_owned(),
            format!("TOON_RELAY_PORT={read}"),
        ],
        None => vec![
            "--network".to_owned(),
            network.to_owned(),
            "--env".to_owned(),
            format!("TOON_BLS_PORT={WRITE_PORT}"),
        ],
    }
}

/// How `docker network create` makes an app's network: internal, unless the app may open
/// connections out.
fn network_create_arguments(egress: bool, network: &str) -> Vec<String> {
    let mut arguments = vec!["network".to_owned(), "create".to_owned()];
    if !egress {
        arguments.push("--internal".to_owned());
    }
    arguments.push(network.to_owned());
    arguments
}

/// What confines every container, the relay's too: no capabilities, the caller's user, a
/// read-only root with `/tmp` and the scratch paths writable, and limits.
fn confinement_arguments(allowances: &Allowances, user: &str) -> Vec<String> {
    let memory = allowances.memory().to_string();
    let mut arguments: Vec<String> = [
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--user",
        user,
        "--read-only",
        "--tmpfs",
        &format!("/tmp:rw,size={TMP_SIZE}"),
    ]
    .iter()
    .map(|argument| (*argument).to_owned())
    .collect();
    for path in &allowances.scratch {
        arguments.extend(["--tmpfs".to_owned(), path.clone()]);
    }
    arguments.extend([
        "--memory".to_owned(),
        memory.clone(),
        // The same as the memory: no swap beyond it.
        "--memory-swap".to_owned(),
        memory,
        "--cpus".to_owned(),
        allowances.cpus().to_string(),
        "--pids-limit".to_owned(),
        allowances.pids().to_string(),
    ]);
    arguments
}

/// The arguments that give a container what `spec` starts it with beyond its data: each
/// copy mounted read-only at its path, and each variable by name, the value being in
/// docker's own environment, so a secret is never in the argument list that `ps` shows.
fn given_arguments(spec: &AppSpec) -> Vec<String> {
    let mounts = spec.mounts.iter().flat_map(|(copy, path)| {
        [
            "--volume".to_owned(),
            format!("{}:{path}:ro", copy.display()),
        ]
    });
    let env = spec
        .env
        .iter()
        .flat_map(|(variable, _)| ["--env".to_owned(), variable.clone()]);
    mounts.chain(env).collect()
}

/// Where the container `name` is on the docker network `network`.
fn container_ip(name: &str, network: &str) -> Result<Ipv4Addr, String> {
    let format = format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}");
    docker(&["inspect", "--format", &format, name])?
        .parse()
        .map_err(|_| format!("docker did not say where {name} is on {network}"))
}

impl AppRunner for ContainerRunner {
    fn start(&self, spec: &AppSpec) -> Result<Box<dyn RunningApp>, Error> {
        let name = format!("toon-{}", spec.instance);
        fs::create_dir_all(&spec.data_dir)
            .map_err(|error| failed(format!("{}: {error}.", spec.data_dir.display())))?;
        let data = fs::canonicalize(&spec.data_dir)
            .map_err(|error| failed(format!("{}: {error}.", spec.data_dir.display())))?;
        let log = app_log(&spec.data_dir);
        // What a supervisor that was killed left behind, its output kept first.
        keep_output(&name, &log);
        let _ = docker(&["rm", "--force", &name]);
        // The network such a container was on, if the supervisor was killed before it went.
        let network = (!spec.relay).then(|| name.clone());
        if let Some(network) = &network {
            let _ = docker(&["network", "rm", network]);
            let arguments = network_create_arguments(spec.allowances.egress, network);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            docker(&arguments).map_err(|why| {
                failed(format!(
                    "docker could not make a network for the app {}: {why}",
                    spec.instance
                ))
            })?;
        }

        let mut command = Command::new("docker");
        // Not `--rm`: a container that exits by itself stays until `stop` has kept its output.
        command.args(["run", "--detach", "--name", &name]);
        // Held until the relay answers, which it does once it has bound them.
        let claimed = if spec.relay {
            Some(free_ports()?)
        } else {
            None
        };
        let relay_ports = claimed
            .as_ref()
            .map(|(write, read)| (write.port, read.port));
        command
            .args(network_arguments(relay_ports, &name))
            .args(confinement_arguments(&spec.allowances, &caller()))
            .arg("--volume")
            .arg(format!("{}:/data", data.display()))
            .args(["--env", "TOON_DATA_DIR=/data"])
            .args(given_arguments(spec));
        // The values `given_arguments` names, for docker to take from this process's environment.
        for (variable, value) in &spec.env {
            command.env(variable, value);
        }
        let output = command
            .arg(&spec.image)
            .output()
            .map_err(|error| failed(format!("docker could not be run: {error}.")))?;
        if !output.status.success() {
            if let Some(network) = &network {
                let _ = docker(&["network", "rm", network]);
            }
            return Err(failed(format!(
                "docker could not start {}: {} {CONFINED}",
                spec.image,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let mut container = Container {
            name: name.clone(),
            network: network.clone(),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            read: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            stopped: false,
            log,
        };
        let reached = match (relay_ports, &network) {
            (Some((write, read)), _) => Ok((
                SocketAddr::from((Ipv4Addr::LOCALHOST, write)),
                SocketAddr::from((Ipv4Addr::LOCALHOST, read)),
            )),
            (None, Some(network)) => container_ip(&name, network).map(|ip| {
                (
                    SocketAddr::from((ip, WRITE_PORT)),
                    SocketAddr::from((ip, READ_PORT)),
                )
            }),
            (None, None) => unreachable!("an app that is not the relay has a network"),
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
                "The app {} ({}): {why}. {logs} {CONFINED}",
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
        keep_output(&self.name, &self.log);
        let _ = docker(&["rm", "--force", &self.name]);
        if let Some(network) = &self.network {
            let _ = docker(&["network", "rm", network]);
        }
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
            mounts: Vec::new(),
            data_dir: data_dir.to_path_buf(),
            relay: false,
            allowances: Allowances::default(),
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

    const USER: &str = "1000:1000";

    fn run_arguments(relay_ports: Option<(u16, u16)>, allowances: &Allowances) -> String {
        let mut arguments = network_arguments(relay_ports, "toon-x");
        arguments.extend(confinement_arguments(allowances, USER));
        arguments.join(" ")
    }

    fn assert_confined(arguments: &str) {
        for expected in [
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--user 1000:1000",
            "--read-only",
            "--tmpfs /tmp:rw,size=64m",
            "--memory 536870912",
            "--memory-swap 536870912",
            "--cpus 1",
            "--pids-limit 256",
        ] {
            assert!(arguments.contains(expected), "{expected}: {arguments}");
        }
        assert!(!arguments.contains("--publish"), "{arguments}");
    }

    #[test]
    fn a_secret_file_is_a_read_only_mount_and_no_value_is_in_the_arguments() {
        let mut spec = spec(Path::new("/home/op/apps/notes/data"), "notes:1");
        spec.env = vec![
            ("LOG_LEVEL".to_owned(), "debug-value".to_owned()),
            ("SIGNING_KEY".to_owned(), "hunter2-value".to_owned()),
        ];
        spec.mounts = vec![(
            PathBuf::from("/home/op/apps/notes/secrets/file-0"),
            "/run/secrets/minter.key".to_owned(),
        )];
        let arguments = given_arguments(&spec).join(" ");
        assert!(
            arguments
                .contains("--volume /home/op/apps/notes/secrets/file-0:/run/secrets/minter.key:ro"),
            "{arguments}"
        );
        assert!(arguments.contains("--env LOG_LEVEL"), "{arguments}");
        assert!(arguments.contains("--env SIGNING_KEY"), "{arguments}");
        for value in ["debug-value", "hunter2-value"] {
            assert!(!arguments.contains(value), "{arguments}");
        }
    }

    #[test]
    fn the_relay_shares_the_hosts_network_and_is_confined() {
        let relay = run_arguments(Some((4100, 4101)), &Allowances::default());
        assert!(relay.contains("--network host"), "{relay}");
        assert!(relay.contains("--env TOON_HOST=127.0.0.1"), "{relay}");
        assert!(relay.contains("--env TOON_WRITE_HOST=127.0.0.1"), "{relay}");
        assert!(relay.contains("--env TOON_BLS_PORT=4100"), "{relay}");
        assert!(relay.contains("--env TOON_RELAY_PORT=4101"), "{relay}");
        assert_confined(&relay);
    }

    #[test]
    fn any_other_app_has_a_network_of_its_own_and_is_confined() {
        let other = run_arguments(None, &Allowances::default());
        assert!(other.contains("--network toon-x"), "{other}");
        assert!(
            other.contains(&format!("--env TOON_BLS_PORT={WRITE_PORT}")),
            "{other}"
        );
        assert!(!other.contains("TOON_HOST"), "{other}");
        assert_confined(&other);
    }

    #[test]
    fn an_apps_network_is_internal_unless_it_may_open_connections_out() {
        let closed = network_create_arguments(false, "toon-x");
        assert_eq!(closed, ["network", "create", "--internal", "toon-x"]);
        let open = network_create_arguments(true, "toon-x");
        assert_eq!(open, ["network", "create", "toon-x"]);
    }

    #[test]
    fn allowances_reach_the_arguments() {
        let allowances = Allowances::parse(
            true,
            &["/cache".into(), "/var/run/".into()],
            Some("1g"),
            Some("2.5"),
            Some("64"),
        )
        .unwrap();
        let arguments = run_arguments(None, &allowances);
        assert!(arguments.contains("--tmpfs /cache"), "{arguments}");
        assert!(arguments.contains("--tmpfs /var/run "), "{arguments}");
        assert!(arguments.contains("--memory 1073741824"), "{arguments}");
        assert!(
            arguments.contains("--memory-swap 1073741824"),
            "{arguments}"
        );
        assert!(arguments.contains("--cpus 2.5"), "{arguments}");
        assert!(arguments.contains("--pids-limit 64"), "{arguments}");
        assert!(
            arguments.contains("--tmpfs /tmp:rw,size=64m"),
            "{arguments}"
        );
    }

    #[test]
    fn a_value_that_cannot_be_used_is_a_usage_error() {
        let refused = |scratch: &[&str], memory, cpus, pids| {
            let scratch: Vec<String> = scratch.iter().map(|path| (*path).to_owned()).collect();
            Allowances::parse(false, &scratch, memory, cpus, pids)
                .err()
                .map(|error| error.code)
        };
        for scratch in ["cache", "/data", "/tmp", "/tmp/", "/", "/a:b", "/a/../b"] {
            assert_eq!(
                refused(&[scratch], None, None, None),
                Some(ErrorCode::Usage)
            );
        }
        for memory in ["0", "lots", "", "0m", "-1"] {
            assert_eq!(
                refused(&[], Some(memory), None, None),
                Some(ErrorCode::Usage)
            );
        }
        for cpus in ["0", "x", "-1", "NaN", "inf"] {
            assert_eq!(refused(&[], None, Some(cpus), None), Some(ErrorCode::Usage));
        }
        for pids in ["0", "x", "1.5"] {
            assert_eq!(refused(&[], None, None, Some(pids)), Some(ErrorCode::Usage));
        }
        assert_eq!(refused(&[], Some("512m"), Some("0.5"), Some("1")), None);
    }

    #[test]
    fn the_confinement_follows_the_allowances() {
        let defaults = Allowances::default().confinement(false, USER);
        assert_eq!(
            defaults,
            serde_json::json!({
                "network": "none", "user": USER, "read_only": true,
                "writable": ["/data", "/tmp"], "capabilities": [],
                "memory": 536870912, "cpus": 1.0, "pids": 256,
            })
        );
        let allowed = Allowances::parse(true, &["/cache".into()], Some("1g"), None, None).unwrap();
        let record = allowed.confinement(false, USER);
        assert_eq!(record["network"], "egress");
        assert_eq!(
            record["writable"],
            serde_json::json!(["/data", "/tmp", "/cache"])
        );
        assert_eq!(record["memory"], 1073741824u64);
        assert_eq!(
            Allowances::default().confinement(true, USER)["network"],
            "host"
        );
    }

    #[test]
    fn the_ports_picked_for_an_app_differ() {
        let (write, read) = free_ports().unwrap();
        assert_ne!(write.port, read.port);
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

    /// An image whose container exits at once keeps what it wrote in the app's log, after
    /// the container is gone. It needs docker and the `hello-world` image.
    #[test]
    #[ignore = "needs docker and a network to pull the hello-world image"]
    fn a_container_that_fails_to_start_keeps_its_output() {
        let data = tempfile::tempdir().unwrap();
        let exits = spec(data.path(), "hello-world");

        let error = ContainerRunner.start(&exits).err().unwrap();

        assert_eq!(error.code, ErrorCode::AppFailed);
        let kept = fs::read_to_string(app_log(data.path())).unwrap();
        assert!(kept.contains("Hello from Docker!"), "{kept}");
        assert!(docker(&["container", "inspect", &format!("toon-{}", exits.instance)]).is_err());
    }
}
