//! A local `anvil` with x402's contracts and a USDC on it: a chain that accepts
//! transactions, shared by every agent node a test starts.
//!
//! The fake chain carries a connector through start and no further. A peering opens and
//! funds a channel, which is a transaction, so a test that peers runs on this.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use connector_settlement_evm::test_support::x402::X402Chain;
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::Address;
use tokio::runtime::Runtime;

/// The token's decimals: USDC's.
pub const TOKEN_DECIMALS: u8 = 6;

/// What `anvil` prints before the address it listens on, once it does.
const LISTENING: &str = "Listening on ";

/// How long `anvil` gets to say where it listens.
const STARTS_WITHIN: Duration = Duration::from_secs(60);

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
        // `anvil` picks the port as it binds it, and says which. A port picked here and
        // handed to it could be another socket's by the time it binds, and a test would
        // then talk to whatever chain is listening there.
        let mut child = Command::new("anvil")
            .args(["--host", "127.0.0.1", "--port", "0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start anvil (is it on PATH? see foundryup)");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let (sender, listening) = mpsc::channel();
        // It logs every request there, so the rest is read too, or it would block.
        thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if let Some(address) = line.trim().strip_prefix(LISTENING) {
                    let _ = sender.send(address.to_owned());
                }
            }
        });
        let Ok(address) = listening.recv_timeout(STARTS_WITHIN) else {
            let _ = child.kill();
            let _ = child.wait();
            panic!("anvil did not say where it listens");
        };
        let rpc_url = format!("http://{address}");
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
