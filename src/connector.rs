//! The embedded connector (ADR 0001): this binary is the connector.
//!
//! `toon connector <config>` is what the supervisor starts as a child process. It does
//! what the connector's own binary does and nothing more: `connector_cli::run` loads the
//! config, builds the runtime and merges the routers, and this serves them.
//!
//! The child speaks to its supervisor on two pipes. On standard output it writes one
//! line, a [`Startup`]. Standard input carries nothing; it is how the child learns the
//! supervisor is gone, because the pipe closes when the supervisor exits, however it
//! exits. Logs go to standard error.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{self, ExitCode};
use std::thread;

use serde_json::{json, Value};

/// The name of the command that serves a connector, as the supervisor spells it.
pub const COMMAND: &str = "connector";

/// The connector revision this binary embeds: the `rev` in `Cargo.toml`, read by
/// `build.rs`.
pub const REVISION: &str = env!("TOON_CONNECTOR_REVISION");

/// The one thing a starting connector tells its supervisor.
pub enum Startup {
    /// The socket is bound, at this address.
    Listening(String),
    /// The connector refused to start, and why.
    Refused(String),
}

impl Startup {
    fn line(&self) -> Value {
        match self {
            Startup::Listening(address) => json!({ "listening": address }),
            Startup::Refused(why) => json!({ "refused": why }),
        }
    }

    /// What the connector said, or `None` if it died without saying anything.
    pub fn heard(line: &str) -> Option<Self> {
        let said: Value = serde_json::from_str(line).ok()?;
        match (said["listening"].as_str(), said["refused"].as_str()) {
            (Some(address), _) => Some(Startup::Listening(address.to_string())),
            (_, Some(why)) => Some(Startup::Refused(why.to_string())),
            _ => None,
        }
    }
}

/// Serve the connector `config` describes until the supervisor is gone.
pub fn serve(config: &Path) -> ExitCode {
    exit_when_the_supervisor_is_gone();
    tracing_subscriber::fmt()
        .json()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            tell_supervisor(Startup::Refused(why));
            ExitCode::FAILURE
        }
    }
}

fn run(config: &Path) -> Result<(), String> {
    let config = config
        .to_str()
        .ok_or_else(|| format!("the config path is not UTF-8: {}", config.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("the async runtime did not start: {error}"))?;

    runtime.block_on(async {
        // `connector_cli::run` takes a process's arguments: the program name, then the
        // config file.
        let connector = match connector_cli::run(&["toon connector", config]).await {
            Ok(connector_cli::Command::Serve(connector)) => connector,
            Ok(connector_cli::Command::Finished { .. }) => {
                return Err(format!(
                    "{config} names a connector verb, not a config file"
                ))
            }
            Err(error) => return Err(error.to_string()),
        };
        let server = axum::Server::try_bind(&connector.client_edge_addr)
            .map_err(|error| format!("{} could not be bound: {error}", connector.client_edge_addr))?
            .serve(connector.router.into_make_service());

        tracing::info!(addr = %server.local_addr(), "connector listening");
        tell_supervisor(Startup::Listening(server.local_addr().to_string()));

        server.await.map_err(|error| error.to_string())
    })
}

fn tell_supervisor(startup: Startup) {
    // A supervisor that is no longer reading is one this process is about to follow.
    let _ = writeln!(io::stdout().lock(), "{}", startup.line());
}

/// A connector must not outlive its supervisor. Nothing is ever written to standard
/// input, so a read returns only when the supervisor's end of the pipe has closed.
fn exit_when_the_supervisor_is_gone() {
    thread::spawn(|| {
        let mut ignored = [0u8; 64];
        let mut stdin = io::stdin().lock();
        while matches!(stdin.read(&mut ignored), Ok(read) if read > 0) {}
        process::exit(0);
    });
}
