//! The fake chain: the connector's own `FakeRpc`, answering as an EVM chain that x402
//! is deployed on and nobody has used yet.
//!
//! A connector with a `[settlement.evm]` table binds to its chain before it serves
//! anything: it reads the chain id, checks that `x402BatchSettlement` is deployed,
//! checks the token's decimals, and asks the contract for the id of a probe channel.
//! Once it is serving, it watches the chain's head and asks what it is owed. This
//! answers those reads the way such a chain would. The channel id is computed by the
//! connector's own `evm_batch_channel_id`, so it is the contract's answer and not a
//! recording of one.
//!
//! The token is a plain ERC-20 with no ERC-3009, so a connector on this chain is
//! configured with `asset_transfer_method = "permit2"`.
//!
//! It holds no channels and accepts no transaction, so it carries a connector that
//! nobody pays. A test that moves money needs more than this. Every address holds a
//! vast balance of gas and of the token, unless the chain was started unfunded and
//! nobody has funded it yet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use connector_chain_rpc::{FakeRpc, RpcCall, RpcReply};
use connector_signer::{evm_batch_channel_id, BatchChannelConfig, BatchSettlementDomain};
use serde_json::json;
use tokio::runtime::Runtime;

/// The chain id the fake reports.
pub const CHAIN_ID: u64 = 31_337;
/// The token the fake chain has, and how many decimals it reports.
pub const TOKEN: &str = "0x00000000000000000000000000000000000000bb";
pub const TOKEN_DECIMALS: u8 = 6;

/// The token's `decimals()`.
const DECIMALS: &str = "313ce567";
/// The contract's `getChannelId(ChannelConfig)`.
/// The token's `balanceOf(owner)`.
const BALANCE_OF: &str = "70a08231";
const GET_CHANNEL_ID: &str = "5e5e0b87";
/// The contract's `receivers(receiver, token)`: what a receiver has claimed and settled.
const RECEIVERS: &str = "21ff6389";
/// A word is 32 bytes: 64 hex digits.
const WORD: usize = 64;

pub struct FakeChain {
    rpc: FakeRpc,
    funded: Arc<AtomicBool>,
    // Dropped after `rpc`, whose server runs on it.
    _runtime: Runtime,
}

impl FakeChain {
    /// A chain on which every address is funded.
    pub fn start() -> Self {
        Self::spawn(true)
    }

    /// A chain on which every address holds nothing until `fund` is called.
    pub fn start_unfunded() -> Self {
        Self::spawn(false)
    }

    /// What a faucet does: from now on every address holds gas and the token.
    pub fn funded(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.funded)
    }

    fn spawn(funded: bool) -> Self {
        let funded = Arc::new(AtomicBool::new(funded));
        let held = Arc::clone(&funded);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime for the fake chain");
        let rpc = runtime.block_on(FakeRpc::spawn(move |call: &RpcCall| {
            answer(call, held.load(Ordering::SeqCst))
        }));
        Self {
            rpc,
            funded,
            _runtime: runtime,
        }
    }

    pub fn rpc_url(&self) -> String {
        self.rpc.url()
    }

    /// How many requests for `method` the chain has answered.
    pub fn count(&self, method: &str) -> usize {
        self.rpc.count(method)
    }
}

fn answer(call: &RpcCall, funded: bool) -> RpcReply {
    match call.method.as_str() {
        "eth_chainId" => RpcReply::Result(json!(format!("{CHAIN_ID:#x}"))),
        "eth_blockNumber" => RpcReply::Result(json!("0x1")),
        // Any code at all: the connector asks only whether the contract is deployed.
        "eth_getCode" => RpcReply::Result(json!("0x60")),
        "eth_getBalance" => RpcReply::Result(json!(if funded {
            "0xde0b6b3a7640000000"
        } else {
            "0x0"
        })),
        "eth_call" => eth_call(call, funded),
        other => not_served(other),
    }
}

fn eth_call(call: &RpcCall, funded: bool) -> RpcReply {
    let request = &call.params[0];
    let data = request["data"]
        .as_str()
        .or_else(|| request["input"].as_str())
        .unwrap_or_default()
        .trim_start_matches("0x");
    let (selector, arguments) = data.split_at(data.len().min(8));
    if selector == DECIMALS {
        return RpcReply::Result(json!(format!("0x{:064x}", TOKEN_DECIMALS)));
    }
    if selector == BALANCE_OF {
        let held: u128 = if funded { 1_000_000_000_000 } else { 0 };
        return RpcReply::Result(json!(format!("0x{held:064x}")));
    }
    if selector == RECEIVERS {
        // Nothing claimed and nothing settled: two zero words.
        return RpcReply::Result(json!(format!("0x{}", "0".repeat(2 * WORD))));
    }
    if selector == GET_CHANNEL_ID && arguments.len() == 7 * WORD {
        let word = |index: usize| unhex(&arguments[index * WORD..(index + 1) * WORD]);
        let address = |index: usize| -> [u8; 20] {
            word(index)[12..]
                .try_into()
                .expect("an address is 20 bytes")
        };
        let config = BatchChannelConfig {
            payer: address(0),
            payer_authorizer: address(1),
            receiver: address(2),
            receiver_authorizer: address(3),
            token: address(4),
            withdraw_delay: u64::from_be_bytes(
                word(5)[24..].try_into().expect("a delay fits 8 bytes"),
            ),
            salt: word(6).try_into().expect("a salt is 32 bytes"),
        };
        let id = evm_batch_channel_id(&BatchSettlementDomain::x402(CHAIN_ID), &config);
        return RpcReply::Result(json!(format!("0x{}", hex(&id))));
    }
    // What a contract does with a call it has no function for.
    RpcReply::Error {
        code: 3,
        message: format!("execution reverted: no function {selector}"),
    }
}

fn not_served(what: &str) -> RpcReply {
    RpcReply::Error {
        code: -32601,
        message: format!("the fake chain does not serve {what}"),
    }
}

fn unhex(digits: &str) -> Vec<u8> {
    (0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&digits[at..at + 2], 16).expect("hex"))
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
