//! A connector that is not peerable: one `toon connector` serves from a config of its
//! own, which says nothing about being peered with, as another operator's might.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};

use tempfile::TempDir;

use super::anvil_chain::AnvilChain;

pub struct Unpeerable {
    child: Child,
    // Closing it is how a connector learns its supervisor is gone.
    _alive: ChildStdin,
    address: String,
    _dir: TempDir,
}

pub fn start(chain: &AnvilChain) -> Unpeerable {
    let dir = tempfile::tempdir().expect("a directory for the connector");
    std::fs::write(dir.path().join("identity.key"), [7u8; 32]).expect("an identity key");
    std::fs::write(dir.path().join("settlement.key"), [8u8; 32]).expect("a settlement key");
    let state = dir.path().join("state");
    std::fs::create_dir(&state).expect("a state directory");
    let config = dir.path().join("connector.toml");
    std::fs::write(
        &config,
        format!(
            "client_edge_addr = \"127.0.0.1:0\"\nstate_dir = {state:?}\n\n\
             [signer]\nkey_file = {:?}\n\n\
             [settlement.evm]\nrpc_url = {:?}\ntoken_address = {:?}\ndecimals = 6\n\
             asset_eip712_name = \"USDC\"\nasset_eip712_version = \"2\"\n\
             asset_transfer_method = \"eip3009\"\n\n\
             [settlement.evm.key]\nkey_file = {:?}\n",
            dir.path().join("identity.key"),
            chain.rpc_url(),
            chain.token(),
            dir.path().join("settlement.key"),
        ),
    )
    .expect("a config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_toon"))
        .arg("connector")
        .arg(&config)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the connector");
    let alive = child.stdin.take().expect("piped stdin");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("piped stdout"))
        .read_line(&mut line)
        .expect("the connector's first line");
    let said: serde_json::Value = serde_json::from_str(&line)
        .unwrap_or_else(|error| panic!("the connector said {line:?} ({error})"));
    let address = said["listening"]
        .as_str()
        .unwrap_or_else(|| panic!("the connector did not listen: {line}"))
        .to_owned();
    Unpeerable {
        child,
        _alive: alive,
        address,
        _dir: dir,
    }
}

impl Unpeerable {
    pub fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }
}

impl Drop for Unpeerable {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
