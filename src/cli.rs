//! The command surface.

use clap::{Parser, Subcommand};

/// The same list as `outcome::Exit` and `docs/exit-codes.md`; `tests/exit_codes.rs`
/// holds the three together.
const EXIT_CODES: &str = "\
Exit codes:
  0  The command did what was asked
  1  The command failed; the error's code says why
  2  The command line was not understood
  3  There is no agent node on this machine";

#[derive(Debug, Parser)]
#[command(
    name = "toon",
    version,
    about = "Runs and manages an agent node",
    after_help = EXIT_CODES
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
}
