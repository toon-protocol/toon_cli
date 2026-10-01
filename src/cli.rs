//! The command surface.

use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::node::{Evm, Options};
use crate::outcome::Exit;

/// What `--version` prints after the name: this release, and the connector it embeds.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (connector ",
    env!("TOON_CONNECTOR_REVISION"),
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
    /// Create the wallet and the first TOON app: the mnemonic is shown once
    Init(InitArgs),
    /// Manage the wallet
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Start the agent node as a `systemd --user` unit that outlives this session
    Up {
        /// Run the supervisor in this process instead of installing the unit
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the agent node, and the unit that `up` installed
    Down,
    /// Show the log of a TOON app or an app
    Logs {
        /// The name of a TOON app, or of an app behind one
        name: String,
        /// How many of the last lines to show
        #[arg(long, short = 'n', default_value_t = crate::status::DEFAULT_LINES)]
        lines: usize,
    },
    /// Serve one connector from its config file: what `up` starts as a child process
    #[command(hide = true)]
    Connector { config: PathBuf },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Where the connector listens; the system picks a port when it is 0
    #[arg(long, default_value = "127.0.0.1:0")]
    pub listen: String,
    /// The EVM chain's JSON-RPC endpoint the connector settles on
    #[arg(long, requires = "evm_token")]
    pub evm_rpc_url: Option<String>,
    /// The token the connector is paid in on that chain
    #[arg(long, requires = "evm_rpc_url")]
    pub evm_token: Option<String>,
    /// The token's decimals
    #[arg(long, default_value_t = 6)]
    pub evm_decimals: u8,
    /// The token's EIP-712 domain name
    #[arg(long, default_value = "USDC")]
    pub evm_asset_name: String,
    /// The token's EIP-712 domain version
    #[arg(long, default_value = "2")]
    pub evm_asset_version: String,
    /// How the token is transferred: `eip3009` or `permit2`
    #[arg(long, default_value = "eip3009")]
    pub evm_transfer_method: String,
}

impl InitArgs {
    pub fn options(&self) -> Options {
        Options {
            listen: self.listen.clone(),
            evm: self
                .evm_rpc_url
                .clone()
                .zip(self.evm_token.clone())
                .map(|(rpc_url, token)| Evm {
                    rpc_url,
                    token,
                    decimals: self.evm_decimals,
                    asset_name: self.evm_asset_name.clone(),
                    asset_version: self.evm_asset_version.clone(),
                    transfer_method: self.evm_transfer_method.clone(),
                }),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum WalletCommand {
    /// List the wallet's addresses by chain
    Show,
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
