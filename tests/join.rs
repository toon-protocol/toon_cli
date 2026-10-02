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
    match network {
        Some((connector, relay)) => node_with(
            chain,
            &[
                "--network",
                "devnet",
                "--connector-url",
                connector,
                "--relay-url",
                relay,
            ],
        ),
        None => node_with(chain, &["--network", "devnet"]),
    }
}

/// An agent node initialised with `network_args` on top of the settings for `chain`.
fn node_with(chain: &AnvilChain, network_args: &[&str]) -> Node {
    let machine = Machine::new();
    let rpc_url = chain.rpc_url();
    let token = chain.token();
    let decimals = support::anvil_chain::TOKEN_DECIMALS.to_string();
    let mut args = network_args.to_vec();
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

#[test]
fn a_network_with_a_connector_and_no_relay_is_joined_and_no_relay_is_read() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    // The mainnet profile names no relay; the anvil token names itself as the devnet's does.
    let agent = node_with(
        &chain,
        &[
            "--network",
            "mainnet",
            "--connector-url",
            &connector,
            "--evm-asset-name",
            "USDC",
        ],
    );

    let joined = agent.machine.toon(&[
        "join",
        "mainnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);

    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert!(joined.json()["relay"].is_null(), "{}", joined.stdout);
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 9);
    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"][0]["id"], "mainnet");
    let after = agent.machine.toon(&["status", "--json"]);
    assert_eq!(after.json()["agent_node"]["joined"], "mainnet");
    assert_eq!(
        after.json()["agent_node"]["reads"].as_array().map(Vec::len),
        Some(0)
    );
}

#[test]
fn a_join_that_finds_its_channel_open_deposits_nothing_and_is_not_counted() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));
    let deposit = DEPOSIT.to_string();

    // An earlier peering under the network's label opened the channel.
    let peered = agent.machine.toon(&[
        "peer",
        "add",
        &connector,
        "--deposit",
        &deposit,
        "--yes",
        "--id",
        "devnet",
        "--json",
    ]);
    assert_eq!(peered.exit_code, 0, "{}", peered.stdout);
    let balance = chain.balance(&agent.evm);
    let remaining = |agent: &Node| {
        agent.machine.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"].clone()
    };
    let left = remaining(&agent);

    std::thread::sleep(std::time::Duration::from_secs(2));
    let joined = agent
        .machine
        .toon(&["join", "devnet", "--deposit", &deposit, "--yes", "--json"]);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_eq!(joined.json()["peering"]["channel"]["status"], "found");
    assert_eq!(joined.json()["deposited"], false);
    assert_eq!(chain.balance(&agent.evm), balance, "nothing was deposited");
    assert_eq!(remaining(&agent), left, "and nothing is counted");
}
