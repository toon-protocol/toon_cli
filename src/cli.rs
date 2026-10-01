//! The command surface.

use std::path::PathBuf;

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::node::{Expiry, Options, Reach};
use crate::outcome::Exit;
use crate::profile::Profile;
use crate::relay;
use crate::spending;

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

    /// The TOON app a command that talks to a connector is about: the first one if omitted.
    /// For `create`, the TOON app the new one is created from
    #[arg(long = "app", id = "toon_app", value_name = "TOON_APP", global = true)]
    pub app: Option<String>,

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
    /// Show or change the spending limit
    Limit {
        #[command(subcommand)]
        command: LimitCommand,
    },
    /// Manage the connector's forwarding routes
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    /// Put an app behind the connector of a TOON app: this restarts that connector
    Add(AddArgs),
    /// Create a second TOON app: a new connector with its own keys, and an app behind it
    Create(CreateArgs),
    /// Stop and remove a TOON app, unless one of its channels still holds funds
    Destroy {
        /// The TOON app's name
        name: String,
    },
    /// Take an app, and its route, away from its connector: this restarts that connector
    Remove {
        /// The app's name
        app: String,
        /// Go ahead although the connector restarts and drops packets in flight
        #[arg(long)]
        yes: bool,
    },
    /// Publish and read Nostr events under the agent identity
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// Scaffold a draft NIP and publish it as an event under the agent identity
    Nip {
        #[command(subcommand)]
        command: NipCommand,
    },
    /// Manage the channels the connector pays and is paid on
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
    },
    /// Set the relay and what a write to it costs
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
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
pub struct AddArgs {
    /// A name for the app, unique in this agent node
    pub app: String,
    /// The TOON app whose connector the app goes behind
    #[arg(long)]
    pub to: String,
    /// A container image to run as the app
    #[arg(long, conflicts_with = "url", required_unless_present = "url")]
    pub image: Option<String>,
    /// The URL of an app you already serve: nothing is run
    #[arg(long)]
    pub url: Option<String>,
    /// The ILP address prefix the connector delivers to the app; `g.toon.<app>` if omitted
    #[arg(long)]
    pub address: Option<String>,
    /// What a client pays the connector for a packet to the app
    #[arg(long, default_value_t = 0)]
    pub price: u64,
    /// Go ahead although the connector restarts and drops packets in flight
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// A name for the TOON app, and for the app behind it, unique in this agent node
    pub name: String,
    /// A container image to run as the app behind the new connector
    #[arg(long, conflicts_with = "url", required_unless_present = "url")]
    pub image: Option<String>,
    /// The URL of an app you already serve: nothing is run
    #[arg(long)]
    pub url: Option<String>,
    /// What a client pays the connector for a packet to the app
    #[arg(long, default_value_t = 0)]
    pub price: u64,
    /// What each of the two channels toward the TOON app it was created from is opened
    /// with, in the token's base units
    #[arg(long, conflicts_with = "no_peer", required_unless_present = "no_peer")]
    pub deposit: Option<u128>,
    /// Create no peerings
    #[arg(long)]
    pub no_peer: bool,
    /// Agree to the Anyone Protocol's terms, which a hidden service needs
    #[arg(long)]
    pub accept_anyone_terms: bool,
    /// Make the TOON app reachable at this public hostname instead of as a hidden service
    #[arg(long, value_name = "HOSTNAME")]
    pub clearnet: Option<String>,
    /// Where the connector listens; the system picks a port when it is 0
    #[arg(long, default_value = "127.0.0.1:0")]
    pub listen: String,
    /// Confirm that the peerings move money: without it nothing is deposited
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Restore the wallet from the mnemonic in `TOON_MNEMONIC_FILE`, else `TOON_MNEMONIC`:
    /// the onion endpoints are new, since a mnemonic does not hold them
    #[arg(long)]
    pub from_mnemonic: bool,
    /// Agree to the Anyone Protocol's terms, which a hidden service needs
    #[arg(long)]
    pub accept_anyone_terms: bool,
    /// Make the TOON app reachable at this public hostname instead of as a hidden service
    #[arg(long, value_name = "HOSTNAME")]
    pub clearnet: Option<String>,
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
    /// The most one command that moves money may pay, in the token's base units
    #[arg(long, default_value_t = spending::DEFAULT_PER_COMMAND)]
    pub max_per_command: u128,
    /// The most the commands of one UTC day may pay together, in the token's base units
    #[arg(long, default_value_t = spending::DEFAULT_PER_DAY)]
    pub max_per_day: u128,
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
            reach: match &self.clearnet {
                Some(hostname) => Reach::Clearnet {
                    hostname: hostname.clone(),
                },
                None => Reach::Hidden,
            },
            accept_anyone_terms: self.accept_anyone_terms,
            listen: self.listen.clone(),
            network: self.network,
            evm: Some(evm),
            solana: self.solana.then(|| self.network.solana()),
            plaintext_peers: self.allow_plaintext_peers,
            limits: spending::Limits {
                per_command: self.max_per_command,
                per_day: self.max_per_day,
            },
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
    /// Confirm that this command moves money: without it nothing is sent
    #[arg(long)]
    pub yes: bool,
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
    /// Confirm that this command moves money: without it nothing is deposited
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum EventCommand {
    /// Sign an event with the agent identity and publish it to the agent node's own relay
    Publish {
        /// The event's kind
        #[arg(long)]
        kind: u64,
        /// The event's content
        #[arg(long, default_value = "")]
        content: String,
        /// The event's tags, as a JSON array of arrays of strings
        #[arg(long, default_value = "[]")]
        tags: String,
        /// What the write is paid, in the token's base units
        #[arg(long, default_value_t = 0, conflicts_with = "relay")]
        amount: u64,
        /// Publish to this relay instead (`ws://host:port`), paying the price its
        /// information document states
        #[arg(long)]
        relay: Option<String>,
        /// Confirm that this command moves money: without it nothing is paid
        #[arg(long, requires = "relay")]
        yes: bool,
    },
    /// Read the stored events of a relay that match a filter
    Query {
        /// The relay's websocket URL, `ws://host:port`
        relay: String,
        /// A NIP-01 filter, as one JSON object
        #[arg(long)]
        filter: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum NipCommand {
    /// Write a draft NIP from the template, named after its title, into the current directory
    New {
        /// The draft's title
        title: String,
    },
    /// Publish a draft as a kind 30817 event, replacing the earlier revision of it
    Publish {
        /// The draft's file, named after its identifier: `<identifier>.md`
        draft: PathBuf,
        /// The agent node's own relay, `ws://host:port`, which the draft is written to: asked
        /// first for the draft's current revision
        #[arg(long)]
        relay: String,
        /// A topic of the draft, in lower case; may be repeated
        #[arg(long = "topic")]
        topics: Vec<String>,
        /// Publish although the relay holds a draft of this identifier under another title
        #[arg(long)]
        title_changed: bool,
        /// What the write is paid, in the token's base units
        #[arg(long, default_value_t = 0)]
        amount: u64,
    },
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
    /// Set what a client pays for a packet on the route to an app: this restarts the connector
    Price {
        /// The ILP address prefix the route terminates at
        prefix: String,
        /// The price, in the token's base units
        price: u64,
        /// Go ahead although the connector restarts and drops packets in flight
        #[arg(long)]
        yes: bool,
    },
    /// Stop forwarding a prefix
    Remove {
        /// The ILP address prefix
        prefix: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum LimitCommand {
    /// Show the spending limit and what is left of today's
    Show,
    /// Change the spending limit; needs the wallet passphrase
    Set {
        /// The most one command that moves money may pay, in the token's base units
        #[arg(long)]
        max_per_command: Option<u128>,
        /// The most the commands of one UTC day may pay together
        #[arg(long)]
        max_per_day: Option<u128>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RelayCommand {
    /// Show the relay's settings and prices, or change them and restart the relay
    Config(RelayConfigArgs),
    /// Set the price of a write on the connector's route, which restarts the connector
    Price {
        /// The price of one write, in the token's base units
        amount: u64,
        /// Restart a running connector without asking: a restart drops the packets it holds
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub struct RelayConfigArgs {
    /// The relay's name; empty unsets it
    #[arg(long)]
    pub name: Option<String>,
    /// The relay's description; empty unsets it
    #[arg(long)]
    pub description: Option<String>,
    /// Whether the relay drops an event once it has expired (`honour`) or keeps it (`ignore`)
    #[arg(long, value_parser = parse_expiry)]
    pub expiry: Option<Expiry>,
    /// Refuse events from this public key, in hex; repeat it for several
    #[arg(long, value_parser = relay::public_key)]
    pub block: Vec<String>,
    /// Stop refusing events from this public key; repeat it for several
    #[arg(long, value_parser = relay::public_key)]
    pub unblock: Vec<String>,
    /// Restart a running relay and its connector without asking: a restart drops the
    /// packets the connector holds
    #[arg(long)]
    pub yes: bool,
}

fn parse_expiry(text: &str) -> Result<Expiry, String> {
    Expiry::from_name(text).ok_or_else(|| "expected `honour` or `ignore`".into())
}

impl RelayConfigArgs {
    pub fn change(&self) -> relay::Change {
        relay::Change {
            name: self.name.clone(),
            description: self.description.clone(),
            expiry: self.expiry,
            block: self.block.clone(),
            unblock: self.unblock.clone(),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum WalletCommand {
    /// List the wallet's addresses by chain
    Show,
    /// Fund the wallet's addresses from the devnet faucet
    Fund,
    /// Show the balance of every address by TOON app and chain
    Balances,
    /// Seal the keystore and every onion endpoint's address key into one file
    Backup {
        /// The file to write; it must not exist
        #[arg(long)]
        out: PathBuf,
    },
    /// Recreate the wallet and the address keys from a backup, in a home with no wallet
    Restore {
        /// The backup `wallet backup` wrote
        file: PathBuf,
    },
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
