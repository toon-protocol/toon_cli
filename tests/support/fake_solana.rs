//! The fake Solana chain: the connector's `FakeRpc`, answering as a Solana RPC endpoint for
//! the two reads `wallet balances` makes, `getBalance` and `getTokenAccountsByOwner`.
//!
//! Every address holds `NATIVE_BALANCE` lamports, and `TOKEN_BALANCE` of the token in one
//! token account, unless the chain was started without token accounts.

use connector_chain_rpc::{FakeRpc, RpcCall, RpcReply};
use serde_json::json;
use tokio::runtime::Runtime;

pub const NATIVE_BALANCE: u64 = 4_000_000_000;
pub const TOKEN_BALANCE: u64 = 7_500_000;

pub struct FakeSolana {
    rpc: FakeRpc,
    // Dropped after `rpc`, whose server runs on it.
    _runtime: Runtime,
}

impl FakeSolana {
    /// A chain on which every address holds SOL and one token account.
    pub fn start() -> Self {
        Self::spawn(true, NATIVE_BALANCE, TOKEN_BALANCE)
    }

    /// A chain on which no address has a token account.
    pub fn start_without_token_accounts() -> Self {
        Self::spawn(false, NATIVE_BALANCE, TOKEN_BALANCE)
    }

    /// A chain on which every address holds `lamports` and `token` base units of the token.
    pub fn start_with(lamports: u64, token: u64) -> Self {
        Self::spawn(true, lamports, token)
    }

    fn spawn(token_account: bool, lamports: u64, token: u64) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime for the fake Solana chain");
        let rpc = runtime.block_on(FakeRpc::spawn(move |call: &RpcCall| {
            match call.method.as_str() {
                "getBalance" => RpcReply::Result(json!({ "context": { "slot": 1 }, "value": lamports })),
                "getTokenAccountsByOwner" => {
                    let accounts = if token_account {
                        vec![json!({ "account": { "data": { "parsed": { "info": {
                            "tokenAmount": { "amount": token.to_string() }
                        } } } } })]
                    } else {
                        vec![]
                    };
                    RpcReply::Result(json!({ "context": { "slot": 1 }, "value": accounts }))
                }
                // The program and the mint a Solana connector checks at boot: one account that is
                // executable and an initialised SPL mint of 6 decimals, owned by the token program.
                "getAccountInfo" => RpcReply::Result(json!({
                    "context": { "slot": 1 },
                    "value": {
                        "lamports": 1,
                        "owner": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
                        "data": ["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAGAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==", "base64"],
                        "executable": true,
                        "rentEpoch": 0,
                    },
                })),
                "getGenesisHash" => {
                    RpcReply::Result(json!("5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"))
                }
                other => RpcReply::Result(json!(format!("{other} is not served"))),
            }
        }));
        Self {
            rpc,
            _runtime: runtime,
        }
    }

    pub fn rpc_url(&self) -> String {
        self.rpc.url()
    }
}
