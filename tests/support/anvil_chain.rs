//! A local `anvil` with x402's contracts and a USDC on it: a chain that accepts
//! transactions, shared by every agent node a test starts.
//!
//! The fake chain carries a connector through start and no further. A peering opens and
//! funds a channel, which is a transaction, so a test that peers runs on this.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use connector_settlement_evm::test_support::x402::X402Chain;
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::Address;
use tokio::runtime::Runtime;

/// The token's decimals: USDC's.
pub const TOKEN_DECIMALS: u8 = 6;

pub struct AnvilChain {
    child: Child,
    rpc_url: String,
    token: Address,
    x402: X402Chain,
    runtime: Runtime,
}

impl AnvilChain {
    /// Start `anvil` on a free port, place x402 on it and deploy a USDC.
    pub fn start() -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|bound| bound.local_addr())
            .expect("a free port")
            .port();
        let rpc_url = format!("http://127.0.0.1:{port}");
        let child = Command::new("anvil")
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start anvil (is it on PATH? see foundryup)");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime for the chain");
        let (x402, token) = runtime.block_on(async {
            let provider = Provider::<Http>::try_from(rpc_url.as_str()).expect("a provider");
            for _ in 0..200 {
                if provider.get_chainid().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let mut x402 = X402Chain::place(&rpc_url).await;
            let token = x402.deploy_fiat_token().await;
            (x402, token)
        });
        Self {
            child,
            rpc_url,
            token,
            x402,
            runtime,
        }
    }

    pub fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    pub fn token(&self) -> String {
        format!("{:#x}", self.token)
    }

    /// Give `address` gas and `amount` of the token.
    pub fn fund(&self, address: &str, amount: u128) {
        let address: Address = address.parse().expect("an EVM address");
        self.runtime.block_on(async {
            self.x402.fund_gas(address).await;
            self.x402.mint(self.token, address, amount).await;
        });
    }

    /// What `address` holds of the token.
    pub fn balance(&self, address: &str) -> u128 {
        let address: Address = address.parse().expect("an EVM address");
        self.runtime
            .block_on(self.x402.balance_of(self.token, address))
    }
}

impl Drop for AnvilChain {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
