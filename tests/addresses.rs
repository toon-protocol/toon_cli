//! The addresses a TOON app answers to sit under a segment of its connector's identity
//! key (ADR 0006).

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

const DEPOSIT: u128 = 1_000_000;

fn shown(machine: &Machine) -> Value {
    machine
        .toon_with(&["wallet", "show", "--json"], |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
        .json()["wallet"]
        .clone()
}

fn identity(machine: &Machine) -> String {
    shown(machine)["connector_identities"][0]["public_key"]
        .as_str()
        .expect("the connector identity")
        .to_owned()
}

fn config(machine: &Machine) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/connector.toml"),
    )
    .unwrap()
}

#[test]
fn two_agent_nodes_have_their_own_segments_taken_from_the_connector_identity() {
    let chain = FakeChain::start();
    let (a, b) = (Machine::new(), Machine::new());
    assert_eq!(a.init_on(&chain).exit_code, 0);
    assert_eq!(b.init_on(&chain).exit_code, 0);
    let _up = a.start(&["up", "--foreground", "--json"]);
    _up.report();

    let segment = a.segment("relay");
    assert_ne!(segment, b.segment("relay"));
    assert_eq!(segment.len(), 16);
    assert!(segment.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert_eq!(segment, identity(&a)[..16]);
    assert_eq!(b.segment("relay"), identity(&b)[..16]);
    assert_eq!(a.relay_prefix(), format!("g.toon.{segment}.relay"));
    let config = config(&a);
    assert!(
        config.contains(&format!("addresses = [\"g.toon.{segment}\"]")),
        "{config}"
    );
    assert!(config.contains(&format!("prefix = \"g.toon.{segment}.relay\"")));
    assert!(!config.contains("\"g.toon.relay\""), "{config}");
    let status = a.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["connector"]["ilp_address"],
        format!("g.toon.{segment}")
    );
}

#[test]
fn an_agent_node_restored_from_its_mnemonic_has_the_same_addresses() {
    let chain = FakeChain::start();
    let before = Machine::new();
    let created = before.init_on(&chain);
    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    let mnemonic = created.json()["mnemonic"].as_str().unwrap().to_owned();

    let after = Machine::new();
    let restored = after.toon_with(
        &[
            "init",
            "--json",
            "--accept-anyone-terms",
            "--from-mnemonic",
            "--evm-rpc-url",
            &chain.rpc_url(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
            command.env("TOON_MNEMONIC", &mnemonic);
        },
    );
    assert_eq!(restored.exit_code, 0, "{}", restored.stdout);

    assert_eq!(after.segment("relay"), before.segment("relay"));
    assert_eq!(after.relay_prefix(), before.relay_prefix());
}

#[test]
fn publishing_with_no_relay_in_the_agent_node_is_not_an_unknown_name() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let path = machine.agent_node_home().join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["toon_apps"][0]["apps"] = serde_json::json!([]);
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let run = machine.toon_with(
        &[
            "event",
            "publish",
            "--kind",
            "1",
            "--content",
            "x",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_ne!(
        run.json()["error"]["code"],
        "unknown_name",
        "{}",
        run.stdout
    );
    // The write is addressed where the relay's route would be, and the connector, which
    // has no such route, rejects it.
    assert_eq!(run.json()["outcome"], "rejected", "{}", run.stdout);
    assert_ne!(run.exit_code, 0);
}

struct Node {
    machine: Machine,
    _up: Foreground,
    address: String,
}

impl Node {
    fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }
}

fn node(chain: &AnvilChain, extra: &[&str]) -> Node {
    let machine = Machine::new();
    let rpc_url = chain.rpc_url();
    let token = chain.token();
    let decimals = support::anvil_chain::TOKEN_DECIMALS.to_string();
    let mut args = extra.to_vec();
    args.extend([
        "--clearnet",
        "toon.example.com",
        "--evm-rpc-url",
        &rpc_url,
        "--evm-token",
        &token,
        "--evm-decimals",
        &decimals,
        "--allow-plaintext-peers",
    ]);
    let init = machine.init_with(&args);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let evm = shown(&machine)["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let address = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .to_owned();
    Node {
        machine,
        _up: up,
        address,
    }
}

/// `from` sends `amount` to `destination`, sealed to the connector of `to`.
fn send(from: &Node, to: &Node, destination: &str, amount: &str) -> Value {
    from.machine
        .toon(&[
            "send",
            destination,
            "--amount",
            amount,
            "--yes",
            "--seal-to",
            &to.url(),
            "--json",
        ])
        .json()
}

#[test]
fn a_joined_agent_node_forwards_the_networks_addresses_to_the_network() {
    let chain = AnvilChain::start();
    let network = node(&chain, &["--network", "devnet"]);
    // An app of the network at an address under `g.toon` that is not its relay's.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/inbox", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            log.lock()
                .unwrap()
                .push(request.lines().next().unwrap_or("").to_owned());
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let added = network.machine.toon(&[
        "add",
        "inbox",
        "--to",
        "relay",
        "--url",
        &url,
        "--address",
        "g.toon.elsewhere",
        "--yes",
        "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}", added.stdout);
    let connector = network.url();
    let agent = node(
        &chain,
        &[
            "--network",
            "devnet",
            "--connector-url",
            &connector,
            "--relay-url",
            "ws://127.0.0.1:7100",
        ],
    );
    let joined = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_ne!(agent.machine.relay_prefix(), network.machine.relay_prefix());

    // The network's relay: fulfilled by the network's connector, not delivered locally.
    let report = send(&agent, &network, &network.machine.relay_prefix(), "1");
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["body"], support::NOT_A_WRITE);

    // Another address under `g.toon`: the network's app answers.
    let report = send(&agent, &network, "g.toon.elsewhere", "0");
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !seen
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("POST /inbox"))
    {
        assert!(
            Instant::now() < deadline,
            "the network's app never received it"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn two_agent_nodes_peered_with_each_other_reach_each_others_relay() {
    let chain = AnvilChain::start();
    let (x, y) = (node(&chain, &[]), node(&chain, &[]));
    for (near, far) in [(&x, &y), (&y, &x)] {
        let peered = near.machine.toon(&[
            "peer",
            "add",
            &far.url(),
            "--deposit",
            &DEPOSIT.to_string(),
            "--yes",
            "--id",
            "other",
            "--json",
        ]);
        assert_eq!(peered.exit_code, 0, "{}", peered.stdout);
        let routed = near.machine.toon(&[
            "route",
            "add",
            &far.machine.relay_prefix(),
            "--peer",
            "other",
            "--json",
        ]);
        assert_eq!(routed.exit_code, 0, "{}", routed.stdout);
    }

    for (near, far) in [(&x, &y), (&y, &x)] {
        let report = send(near, far, &far.machine.relay_prefix(), "1");
        assert_eq!(report["outcome"], "fulfilled", "{report}");
        assert_eq!(report["response"]["body"], support::NOT_A_WRITE);
    }
}
