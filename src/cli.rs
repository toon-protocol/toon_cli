//! The command surface.

use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::node::Options;
use crate::outcome::Exit;
use crate::profile::Profile;

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
    /// Create the wallet and the first TOON app: the mnemonic is shown once
    Init(InitArgs),
    /// Manage the wallet
    Wallet {
        #[command(subcommand)]
        command: WalletCommand,
    },
    /// Send one packet to an address and say whether it was fulfilled or rejected
    Send(SendArgs),
    /// Manage peerings: the connectors this one forwards packets to
    Peer {
        #[command(subcommand)]
        command: PeerCommand,
    },
    /// Manage the connector's forwarding routes
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    /// Manage the channels the connector pays and is paid on
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
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
    /// Let the connector peer toward a plain `http://` address, for a trial on one machine
    #[arg(long)]
    pub allow_plaintext_peers: bool,
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
            plaintext_peers: self.allow_plaintext_peers,
            faucet_url: self
                .faucet_url
                .clone()
                .or_else(|| self.network.faucet_url().map(str::to_owned)),
        }
    }
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// The ILP address the packet is bound for
    pub address: String,
    /// The amount, in the token's base units: a send always states it
    #[arg(long)]
    pub amount: u64,
    /// The `/ilp` URL of the connector that terminates the packet, when it is not this
    /// one: the payload is sealed to that connector's identity
    #[arg(long)]
    pub seal_to: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum PeerCommand {
    /// Peer toward another connector: open and fund the channel it is paid on
    Add(PeerAddArgs),
    /// List the peerings
    List,
    /// Remove a peering
    Remove {
        /// The peering's label, as `peer list` shows it
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct PeerAddArgs {
    /// The other connector's address: the URL of its `/ilp` endpoint
    pub address: String,
    /// What the channel is opened with, in the token's base units
    #[arg(long)]
    pub deposit: u128,
    /// A label for the peering; the address, reduced to letters and digits, if omitted
    #[arg(long)]
    pub id: Option<String>,
    /// What this connector keeps of each packet it forwards over the peering
    #[arg(long, default_value_t = 0)]
    pub fee: u64,
    /// The most one forwarded packet may carry; the connector's default if omitted
    #[arg(long, default_value_t = 0)]
    pub max_packet_amount: u64,
}

#[derive(Debug, Subcommand)]
pub enum RouteCommand {
    /// List the connector's routes: the ones it terminates and the ones it forwards
    List,
    /// Forward packets for a prefix to a peering
    Add {
        /// The ILP address prefix to forward
        prefix: String,
        /// The peering's label, as `peer list` shows it
        #[arg(long)]
        peer: String,
        /// What a client pays the connector for a packet on this route
        #[arg(long, default_value_t = 0)]
        price: u64,
    },
    /// Stop forwarding a prefix
    Remove {
        /// The ILP address prefix
        prefix: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum WalletCommand {
    /// List the wallet's addresses by chain
    Show,
    /// Fund the wallet's addresses from the devnet faucet
    Fund,
    /// Show the balance of every address by TOON app and chain
    Balances,
}

#[derive(Debug, Subcommand)]
pub enum ChannelCommand {
    /// List the channels in both directions, with collateral and status
    List,
    /// Open an outbound channel toward a counterparty and deposit into it
    Open {
        /// A file holding the counterparty's `batchSettlements` entry for one chain, as its
        /// self-description publishes it
        #[arg(long)]
        terms: PathBuf,
        /// The opening deposit, in the token's base units
        #[arg(long)]
        deposit: u128,
        /// The counterparty's URL, which a Solana sponsor endpoint published as a path resolves against
        #[arg(long)]
        url: Option<String>,
    },
    /// Deposit more into an outbound channel
    Fund {
        /// The channel's id, as `channel list` shows it
        id: String,
        /// The amount to add, in the token's base units
        #[arg(long)]
        amount: u128,
    },
    /// Start or finish withdrawing an outbound channel's collateral; the chain decides which
    Withdraw {
        /// The channel's id, as `channel list` shows it
        id: String,
    },
    /// Land the latest voucher held on an inbound channel now
    Land {
        /// The channel's id, as `channel list` shows it
        id: String,
    },
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
