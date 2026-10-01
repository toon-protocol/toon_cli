//! The command surface.

use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::node::Options;
use crate::outcome::Exit;
use crate::profile::Profile;

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
    /// Run the agent node in the foreground
    Up,
    /// Stop the agent node that `up` runs
    Down,
    /// Serve one connector from its config file: what `up` starts as a child process
    #[command(hide = true)]
    Connector { config: PathBuf },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Where the connector listens; the system picks a port when it is 0
    #[arg(long, default_value = "127.0.0.1:0")]
    pub listen: String,
    /// The network profile the chain settings come from
    #[arg(long, value_enum, default_value_t = Profile::Devnet)]
    pub network: Profile,
    /// Settle on Solana too: its key then needs SOL and the token before `up`
    #[arg(long)]
    pub solana: bool,
    /// Where `wallet fund` asks for funds, instead of the profile's faucet
    #[arg(long)]
    pub faucet_url: Option<String>,
    /// The EVM chain's JSON-RPC endpoint, instead of the profile's
    #[arg(long)]
    pub evm_rpc_url: Option<String>,
    /// The token the connector is paid in on that chain, instead of the profile's
    #[arg(long)]
    pub evm_token: Option<String>,
    /// The token's decimals, at most 18
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=18))]
    pub evm_decimals: Option<u8>,
    /// The token's EIP-712 domain name
    #[arg(long)]
    pub evm_asset_name: Option<String>,
    /// The token's EIP-712 domain version
    #[arg(long)]
    pub evm_asset_version: Option<String>,
    /// How the token is transferred: `eip3009` or `permit2`
    #[arg(long)]
    pub evm_transfer_method: Option<String>,
}

impl InitArgs {
    pub fn options(&self) -> Options {
        let mut evm = self.network.evm();
        if let Some(rpc_url) = &self.evm_rpc_url {
            evm.rpc_url = rpc_url.clone();
        }
        if let Some(token) = &self.evm_token {
            evm.token = token.clone();
        }
        if let Some(decimals) = self.evm_decimals {
            evm.decimals = decimals;
        }
        if let Some(name) = &self.evm_asset_name {
            evm.asset_name = name.clone();
        }
        if let Some(version) = &self.evm_asset_version {
            evm.asset_version = version.clone();
        }
        if let Some(method) = &self.evm_transfer_method {
            evm.transfer_method = method.clone();
        }
        Options {
            listen: self.listen.clone(),
            network: self.network,
            evm: Some(evm),
            solana: self.solana.then(|| self.network.solana()),
            faucet_url: self
                .faucet_url
                .clone()
                .or_else(|| self.network.faucet_url().map(str::to_owned)),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum WalletCommand {
    /// List the wallet's addresses by chain
    Show,
    /// Fund the wallet's addresses from the devnet faucet
    Fund,
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
