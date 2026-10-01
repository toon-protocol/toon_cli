mod support;

use support::anvil_chain::AnvilChain;
use support::{Foreground, Machine};

/// What the channel toward the network's connector is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

struct Node {
    machine: Machine,
    _up: Foreground,
    address: String,
    evm: String,
}

/// An agent node that joins a network whose connector is `network`'s, if there is one.
fn node_on(chain: &AnvilChain, network: Option<(&str, &str)>) -> Node {
    let machine = Machine::new();
    let rpc_url = chain.rpc_url();
    let token = chain.token();
    let decimals = support::anvil_chain::TOKEN_DECIMALS.to_string();
    let mut args = vec![
        "--network",
        "devnet",
        "--clearnet",
        "toon.example.com",
        "--evm-rpc-url",
        &rpc_url,
        "--evm-token",
        &token,
        "--evm-decimals",
        &decimals,
        "--allow-plaintext-peers",
    ];
    if let Some((connector, relay)) = network {
        args.extend(["--connector-url", connector, "--relay-url", relay]);
    }
    let init = machine.init_with(&args);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
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
        evm,
    }
}

#[test]
fn a_new_agent_node_is_unconnected_until_it_joins_and_then_reads_the_networks_relay() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let before = agent.machine.toon(&["status"]);
    assert!(before.stdout.contains("Unconnected"), "{}", before.stdout);
    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"].as_array().map(Vec::len), Some(0));
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10, "nothing is spent");

    let unconfirmed = agent
        .machine
        .toon(&["join", "devnet", "--deposit", &DEPOSIT.to_string()]);
    assert_eq!(unconfirmed.exit_code, 1);
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10);

    let joined = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_eq!(joined.json()["relay"], "ws://127.0.0.1:7100");
    assert_eq!(
        chain.balance(&agent.evm),
        DEPOSIT * 9,
        "the deposit is on chain"
    );

    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"][0]["id"], "devnet");
    let routes = agent.machine.toon(&["route", "list"]);
    assert!(routes.stdout.contains("g.toon"), "{}", routes.stdout);

    let after = agent.machine.toon(&["status", "--json"]);
    assert_eq!(
        after.json()["agent_node"]["reads"][0],
        "ws://127.0.0.1:7100"
    );
    assert_eq!(after.json()["agent_node"]["joined"], "devnet");

    let left = agent.machine.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .clone();
    let again = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(again.json()["error"]["code"], "join_refused");
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 9);
    let still = agent.machine.toon(&["limit", "show", "--json"]);
    assert_eq!(
        still.json()["limits"]["remaining_today"],
        left,
        "a refused join is not counted"
    );
}

#[test]
fn join_over_the_per_command_limit_is_refused_and_spends_nothing() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        "999999999999",
        "--yes",
        "--json",
    ]);
    assert_eq!(joined.json()["error"]["code"], "spending_limit");
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10);
}
