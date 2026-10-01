//! The command surface.

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::outcome::Exit;

/// What `--version` prints after the name: this release, the connector it embeds and the
/// relay image it runs.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (connector ",
    env!("TOON_CONNECTOR_REVISION"),
    ", relay ",
    env!("TOON_RELAY_IMAGE"),
    ")"
);

#[derive(Debug, Parser)]
#[command(
    name = "toon",
    version = VERSION,
    about = "Runs and manages an agent node",
    // `--help` is the one way to ask for help, so that `help` is not a command that
    // would have to accept `--json` like every other.
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Print one JSON document instead of text
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the agent node on this machine
    Status,
    /// Run the agent node in the foreground
    Up,
    /// Serve one connector from its config file: what `up` starts as a child process
    #[command(hide = true)]
    Connector { config: PathBuf },
}

impl Cli {
    /// Parse this process's command line. `--help` ends with the exit codes.
    pub fn from_command_line() -> Result<Self, clap::Error> {
        let exit_codes: String = Exit::ALL
            .iter()
            .map(|exit| format!("\n  {}  {}", *exit as u8, exit.meaning()))
            .collect();
        let matches = Self::command()
            .after_help(format!("Exit codes:{exit_codes}"))
            .try_get_matches()?;
        Self::from_arg_matches(&matches)
    }
}
