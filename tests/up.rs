mod support;

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use connector_signer::{LocalSigner, Signer};
use serde_json::Value;
use support::fake_chain::{self, FakeChain};
use support::Machine;

/// The connector's identity key in these tests, as the 32 raw bytes of its key file.
const IDENTITY_KEY: [u8; 32] = [7; 32];
const SETTLEMENT_KEY: [u8; 32] = [9; 32];

/// Give `machine` the connector config `toon up` starts from: one connector on
/// loopback, on a port the system picks, settling on `chain`. A connector that settles
/// keeps its journals in a state directory, so it has one.
fn configure_a_connector(machine: &Machine, chain: &FakeChain) {
    let identity_key = machine.write_agent_node_file("identity.key", IDENTITY_KEY);
    let settlement_key = machine.write_agent_node_file("settlement.key", SETTLEMENT_KEY);
    machine.write_agent_node_file(
        "connector.toml",
        format!(
            r#"
client_edge_addr = "127.0.0.1:0"
state_dir = "{state_dir}"

[signer]
key_file = "{identity_key}"

[settlement.evm]
rpc_url = "{rpc_url}"
token_address = "{token}"
decimals = {decimals}
asset_eip712_name = "USDC"
asset_eip712_version = "2"
asset_transfer_method = "permit2"

[settlement.evm.key]
key_file = "{settlement_key}"
"#,
            state_dir = machine.agent_node_home().join("state").display(),
            identity_key = identity_key.display(),
            settlement_key = settlement_key.display(),
            rpc_url = chain.rpc_url(),
            token = fake_chain::TOKEN,
            decimals = fake_chain::TOKEN_DECIMALS,
        ),
    );
}

/// Where the connector in `report` listens.
fn address(report: &Value) -> SocketAddr {
    report["connector"]["address"]
        .as_str()
        .and_then(|address| address.parse().ok())
        .unwrap_or_else(|| panic!("the report has no connector address: {report}"))
}

fn get(address: SocketAddr, path: &str) -> Value {
    reqwest::blocking::get(format!("http://{address}{path}"))
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .unwrap_or_else(|error| panic!("GET {path} from the connector: {error}"))
}

#[test]
fn up_starts_a_connector_that_answers_its_identity_endpoint_on_loopback() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--json"]);
    let report = up.report();

    let address = address(&report);
    assert!(
        address.ip().is_loopback(),
        "the connector listens on loopback: {report}"
    );
    let identity = get(address, "/ilp/identity");
    let key = LocalSigner::from_secret_bytes("expected", IDENTITY_KEY)
        .and_then(|signer| signer.public_key())
        .expect("the public half of the identity key");
    assert_eq!(
        identity["publicKey"],
        format!("0x{}", fake_chain::hex(key.as_ref()))
    );
    assert_eq!(up.stderr(), "");
}

#[test]
fn the_connector_settles_on_the_fake_chain() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--json"]);
    let report = up.report();

    // The connector's self-description publishes the chain it bound to and the token
    // it takes there, which it reads off the chain and not out of its config.
    let address = address(&report);
    let description = get(address, "/ilp");
    let settlement = &description["batchSettlements"][0];
    assert_eq!(
        settlement["network"],
        format!("eip155:{}", fake_chain::CHAIN_ID)
    );
    assert_eq!(settlement["asset"], fake_chain::TOKEN);
    assert!(chain.count("eth_chainId") >= 1);
}

#[test]
fn the_connector_is_a_child_process_of_the_same_binary() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--json"]);
    let report = up.report();

    let connector = report["connector"]["pid"].as_u64().expect("a pid");
    let process = Path::new("/proc").join(connector.to_string());
    let stat = fs::read_to_string(process.join("stat")).expect("the connector is running");
    // `pid (command) state ppid ...`, and the command may itself contain spaces.
    let parent = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split(' ').nth(1))
        .expect("a parent pid");
    assert_eq!(parent, up.pid().to_string());
    assert_eq!(
        fs::read_link(process.join("exe")).expect("the connector's binary"),
        fs::canonicalize(env!("CARGO_BIN_EXE_toon")).expect("the toon binary")
    );
}

#[test]
fn the_connector_does_not_outlive_its_supervisor() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);
    let mut up = machine.start(&["up", "--json"]);
    let report = up.report();
    let address = address(&report);
    assert!(TcpStream::connect(address).is_ok());

    up.kill();

    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(address).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the connector is still listening after its supervisor was killed"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn up_fails_when_its_connector_stops() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);
    let mut up = machine.start(&["up", "--json"]);
    let connector = up.report()["connector"]["pid"].as_u64().expect("a pid");

    let killed = Command::new("kill")
        .arg(connector.to_string())
        .status()
        .expect("run kill");
    assert!(killed.success());

    assert_eq!(up.exit_code(), 1);
    assert_eq!(up.stderr(), "");
}

#[test]
fn up_is_readable_text_without_json() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up"]);

    let line = up.line();
    assert!(
        line.starts_with("Connector listening on 127.0.0.1:"),
        "stdout: {line}"
    );
    assert_eq!(up.stderr(), "");
}

#[test]
fn up_reports_the_connector_revision_it_runs() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--json"]);

    assert_eq!(
        up.report()["connector"]["revision"],
        machine.toon(&["--version", "--json"]).json()["connector_revision"]
    );
}

#[test]
fn up_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["up", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.stderr, "");
}

#[test]
fn up_fails_with_the_connectors_reason_when_it_refuses_its_config() {
    let machine = Machine::new();
    machine.write_agent_node_file(
        "connector.toml",
        r#"
client_edge_addr = "127.0.0.1:0"

[signer]
key_file = "/nonexistent/identity.key"
"#,
    );

    let run = machine.toon(&["up", "--json"]);

    let error = &run.json()["error"];
    assert_eq!(error["code"], "connector_failed");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("/nonexistent/identity.key")),
        "the message carries the connector's own refusal: {error}"
    );
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}
