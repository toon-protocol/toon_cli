//! A disposable local chain for a test that moves money: `anvil`, with x402's contracts
//! placed on it and a USDC deployed, as the connector's own tests have it.
//!
//! `anvil` comes with Foundry. Where it is missing, a test skips, except under CI, where
//! it fails: `available` says which.

use connector_settlement_evm::test_support::x402::X402Chain;
use connector_settlement_evm::test_support::{require_anvil, Anvil};
use ethers::types::Address;
use tokio::runtime::Runtime;

/// This test binary's base port for `anvil`, clear of the connector's own tests' ranges.
const BASE_PORT: u16 = 24_700;
/// The token's decimals: USDC's.
pub const TOKEN_DECIMALS: u8 = 6;

pub struct LocalChain {
    x402: X402Chain,
    token: Address,
    anvil: Anvil,
    runtime: Runtime,
}

impl LocalChain {
    /// Whether this machine can start one. False only outside CI.
    pub fn available() -> bool {
        require_anvil()
    }

    pub fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime for the local chain");
        let anvil = runtime.block_on(Anvil::spawn(BASE_PORT));
        let mut x402 = runtime.block_on(X402Chain::place(&anvil.rpc_url));
        let token = runtime.block_on(x402.deploy_fiat_token());
        Self {
            x402,
            token,
            anvil,
            runtime,
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.anvil.rpc_url
    }

    /// The USDC's address, as `0x` and 40 lowercase hex.
    pub fn token(&self) -> String {
        format!("{:#x}", self.token)
    }

    /// Give `address` gas, and `amount` of the USDC.
    pub fn fund(&self, address: &str, amount: u128) {
        let address: Address = address.parse().expect("an EVM address");
        self.runtime.block_on(async {
            self.x402.fund_gas(address).await;
            self.x402.mint(self.token, address, amount).await;
        });
    }

    /// What `address` holds of the USDC.
    pub fn balance(&self, address: &str) -> u128 {
        let address: Address = address.parse().expect("an EVM address");
        self.runtime
            .block_on(self.x402.balance_of(self.token, address))
    }

    /// Move the chain's clock on by `seconds`.
    pub fn advance_time(&self, seconds: u64) {
        self.runtime.block_on(self.x402.advance_time(seconds));
    }
}
