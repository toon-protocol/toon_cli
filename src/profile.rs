//! Network profiles: the chain settings an operator would otherwise write by hand.
//!
//! A profile is a starting point. A flag of `toon init` that names a chain setting
//! replaces that one setting, and what results is what `state.json` records.

use clap::ValueEnum;

use crate::node::{Evm, Solana};

/// Where an agent node settles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    /// Base Sepolia and Solana devnet, funded from the devnet faucet. The default, so
    /// that a first run costs no real money.
    #[default]
    Devnet,
    /// The local sandbox's anvil and Solana validator, on this machine.
    Sandbox,
    /// Base and Solana mainnet-beta, with real money.
    Mainnet,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Devnet => "devnet",
            Profile::Sandbox => "sandbox",
            Profile::Mainnet => "mainnet",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Profile::Devnet, Profile::Sandbox, Profile::Mainnet]
            .into_iter()
            .find(|profile| profile.name() == name)
    }

    pub fn evm(self) -> Evm {
        let (rpc_url, token, asset_name) = match self {
            Profile::Devnet => (
                "https://base-sepolia-rpc.publicnode.com",
                "0x0C996d7c934c79a6255254875607Fe69df25C0E1",
                "USDC",
            ),
            Profile::Sandbox => (
                "http://localhost:8545",
                "0x0A867CA0442383c2A89951244B955AA19b615b58",
                "USDC",
            ),
            // Base's native USDC names itself "USD Coin", not "USDC".
            Profile::Mainnet => (
                "https://mainnet.base.org",
                "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                "USD Coin",
            ),
        };
        Evm {
            rpc_url: rpc_url.into(),
            token: token.into(),
            decimals: 6,
            asset_name: asset_name.into(),
            asset_version: "2".into(),
            transfer_method: "eip3009".into(),
        }
    }

    pub fn solana(self) -> Solana {
        let (rpc_url, token) = match self {
            Profile::Devnet => (
                "https://api.devnet.solana.com",
                "34eSxY7qxQ4GzyhDJ8GpUcTz1WWzruGbJbR8q6TtxfQU",
            ),
            Profile::Sandbox => (
                "http://localhost:8899",
                "H8HSreUF2s8r8hem4qMttE3bWYCpFuh71jbuos5bA77H",
            ),
            Profile::Mainnet => (
                "https://api.mainnet-beta.solana.com",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            ),
        };
        Solana {
            rpc_url: rpc_url.into(),
            token: token.into(),
            decimals: 6,
        }
    }

    /// The faucet that funds this network's addresses. Only the devnet has one.
    pub fn faucet_url(self) -> Option<&'static str> {
        match self {
            Profile::Devnet => Some("https://faucet.devnet.toonprotocol.dev"),
            Profile::Sandbox | Profile::Mainnet => None,
        }
    }

    /// Whether the connector peers toward a plain `http://` address without being asked:
    /// every endpoint of the sandbox is one.
    pub fn plaintext_peers(self) -> bool {
        self == Profile::Sandbox
    }

    /// The `/ilp` URL of the connector `toon join` peers toward.
    /// Mainnet has none: no mainnet TOON network exists yet.
    pub fn connector_url(self) -> Option<&'static str> {
        match self {
            Profile::Devnet => Some("https://proxy.relay.devnet.toonprotocol.dev/ilp"),
            Profile::Sandbox => Some("http://localhost:3200/ilp"),
            Profile::Mainnet => None,
        }
    }

    /// The connector an agent node records when `init` names none. The sandbox's hub is on
    /// `localhost`, which a hidden agent node cannot reach, so it records none there.
    pub fn default_connector_url(self, hidden: bool) -> Option<&'static str> {
        match self {
            Profile::Sandbox if hidden => None,
            _ => self.connector_url(),
        }
    }

    /// The websocket URL of the relay `toon join` makes one the agent reads.
    /// Mainnet has none: no mainnet TOON network exists yet.
    pub fn relay_url(self) -> Option<&'static str> {
        match self {
            Profile::Devnet => Some("wss://relay-ws.devnet.toonprotocol.dev"),
            Profile::Sandbox => Some("ws://localhost:7100"),
            Profile::Mainnet => None,
        }
    }
}
