//! `toon up`: the supervisor, in the foreground.
//!
//! It starts one connector as a child process of this same binary (`connector`), from
//! the connector config in the agent node's home. No command writes that config yet;
//! `init` brings the state it will be rendered from.

use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use serde_json::json;

use crate::connector::{self, Startup};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// The config the connector is started from, under the agent node's home.
const CONNECTOR_CONFIG: &str = "connector.toml";
/// Where the connector's logs go, under the agent node's home.
const CONNECTOR_LOG: &str = "connector.log";

/// A supervisor whose connector is listening.
pub struct Supervisor {
    connector: Child,
    /// The connector exits when this closes, so it is held for as long as the
    /// supervisor lives and is never written to.
    _alive: ChildStdin,
    address: String,
    home: PathBuf,
    log: PathBuf,
}

/// Start the connector of the agent node at `home`, and return once it is listening.
pub fn start(home: &Path) -> Result<Supervisor, Error> {
    let config = home.join(CONNECTOR_CONFIG);
    if !config.is_file() {
        return Err(Error {
            code: ErrorCode::NoAgentNode,
            message: format!(
                "No agent node at {}: there is no connector config at {}.",
                home.display(),
                config.display()
            ),
        });
    }
    let failed = |message: String| Error {
        code: ErrorCode::ConnectorFailed,
        message,
    };

    let log = home.join(CONNECTOR_LOG);
    let logs = File::options()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|error| failed(format!("The connector's log could not be opened: {error}.")))?;
    let toon = env::current_exe()
        .map_err(|error| failed(format!("This binary could not find itself: {error}.")))?;
    let mut child = Command::new(toon)
        .arg(connector::COMMAND)
        .arg(&config)
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
            return Ok(Supervisor {
                connector: child,
                _alive: alive,
                address,
                home: home.to_path_buf(),
                log,
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

impl Supervisor {
    /// What is running.
    pub fn report(&self) -> Report {
        let pid = self.connector.id();
        let address = &self.address;
        let revision = connector::REVISION;
        let home = self.home.to_string_lossy();
        let log = self.log.to_string_lossy();
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
            }),
            text: format!(
                "Connector listening on {address} (pid {pid}, revision {revision}). \
                 Its log is {log}."
            ),
        }
    }

    /// Stay in the foreground for as long as the connector runs. A supervisor has
    /// nothing else to wait for, so a connector that stops is a failure.
    pub fn wait(mut self) -> Error {
        let how = match self.connector.wait() {
            Ok(status) => status.to_string(),
            Err(error) => error.to_string(),
        };
        Error {
            code: ErrorCode::ConnectorFailed,
            message: format!("The connector stopped ({how})."),
        }
    }
}
