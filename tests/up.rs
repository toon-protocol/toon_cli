mod support;

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::fake_chain::{self, FakeChain};
use support::Machine;

/// Give `machine` an agent node whose connector settles on `chain`, through `toon init`.
fn configure_a_connector(machine: &Machine, chain: &FakeChain) {
    let init = machine.init_on(chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
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

    let up = machine.start(&["up", "--foreground", "--json"]);
    let report = up.report();

    let address = address(&report);
    assert!(
        address.ip().is_loopback(),
        "the connector listens on loopback: {report}"
    );
    // The key the connector serves is the wallet's identity key for connector 0, the
    // one `toon wallet show` lists.
    let identity = get(address, "/ilp/identity");
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let expected = shown.json()["wallet"]["connector_identities"][0]["public_key"].clone();
    let served = identity["publicKey"].as_str().expect("a public key");
    // Served uncompressed, `0x04` then x then y; the wallet lists x alone.
    assert_eq!(
        served.get(..68).and_then(|x| x.strip_prefix("0x04")),
        expected.as_str(),
        "{served} does not carry {expected}"
    );
    assert_eq!(up.stderr(), "");
}

#[test]
fn the_connector_settles_on_the_fake_chain() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--foreground", "--json"]);
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

    let up = machine.start(&["up", "--foreground", "--json"]);
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
    let mut up = machine.start(&["up", "--foreground", "--json"]);
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
fn up_is_readable_text_without_json() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);

    let up = machine.start(&["up", "--foreground"]);

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

    let up = machine.start(&["up", "--foreground", "--json"]);

    assert_eq!(
        up.report()["connector"]["revision"],
        machine.toon(&["--version", "--json"]).json()["connector_revision"]
    );
}

#[test]
fn up_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.stderr, "");
}

#[test]
fn up_fails_with_the_connectors_reason_when_the_chain_is_not_there() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    configure_a_connector(&machine, &chain);
    drop(chain);

    let run = machine.toon(&["up", "--foreground", "--json"]);

    let error = &run.json()["error"];
    assert_eq!(error["code"], "connector_failed");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("refused to start")),
        "the message carries the connector's own refusal: {error}"
    );
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}
