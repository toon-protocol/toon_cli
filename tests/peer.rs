mod support;

use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run};

/// What the peering's channel is opened with, in the token's base units: one USDC.
const DEPOSIT: u128 = 1_000_000;

/// One agent node on the shared chain, running.
struct Node {
    machine: Machine,
    _up: Foreground,
    /// Where its connector listens, as `up` reports it.
    address: String,
    /// Its wallet's EVM address, which a peering's deposit is paid from.
    evm: String,
}

impl Node {
    /// The URL a peer names to peer toward this connector.
    fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }

    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon(args)
    }
}

/// An agent node with the fake relay behind its connector, whose wallet holds
/// `DEPOSIT * 10`.
fn node_on(chain: &AnvilChain) -> Node {
    node_with(chain, true)
}

/// Like `node_on`, saying whether its connector may peer toward a plain `http://` address.
fn node_with(chain: &AnvilChain, plaintext_peers: bool) -> Node {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, plaintext_peers);
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
fn one_operator_peers_alone_and_a_packet_crosses_and_is_fulfilled() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);

    let peered = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--id",
        "far",
        "--json",
    ]);
    assert_eq!(peered.exit_code, 0, "{}", peered.stdout);
    let peering = peered.json()["peering"].clone();
    assert_eq!(peering["id"], "far");
    assert_eq!(peering["channel"]["status"], "created");
    assert_eq!(
        chain.balance(&near.evm),
        DEPOSIT * 9,
        "the deposit is on chain"
    );

    let routed = near.toon(&[
        "route",
        "add",
        "g.toon.relay.far",
        "--peer",
        "far",
        "--json",
    ]);
    assert_eq!(routed.exit_code, 0, "{}", routed.stdout);

    // The far connector charges the relay's write price, so the packet carries it.
    let sent = near.toon(&[
        "send",
        "g.toon.relay.far",
        "--amount",
        "1",
        "--yes",
        "--seal-to",
        &far.url(),
        "--json",
    ]);
    let report = sent.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["body"], "stored");
    assert_eq!(sent.exit_code, 0);

    // The other operator did nothing, and has nothing to forward back over.
    let theirs = far.toon(&["peer", "list", "--json"]);
    assert_eq!(theirs.json()["peers"].as_array().map(Vec::len), Some(0));
}

#[test]
fn peer_add_says_the_other_connector_forwards_back_only_if_its_operator_peers_in_return() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);

    let peered = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
    ]);

    assert_eq!(peered.exit_code, 0, "{}", peered.stderr);
    assert!(
        peered.stdout.contains(
            "forwards back to you only if its operator creates a peering toward you in return"
        ),
        "{}",
        peered.stdout
    );
}

#[test]
fn peers_and_routes_are_listed_and_removed() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let added = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--id",
        "far",
    ]);
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    let routed = near.toon(&["route", "add", "g.toon.relay.far", "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);

    let peers = near.toon(&["peer", "list", "--json"]).json()["peers"].clone();
    assert_eq!(peers.as_array().map(Vec::len), Some(1), "{peers}");
    assert_eq!(peers[0]["id"], "far");
    let routes = near.toon(&["route", "list", "--json"]).json();
    assert_eq!(routes["forwarding_routes"][0]["prefix"], "g.toon.relay.far");
    assert_eq!(routes["forwarding_routes"][0]["peer_id"], "far");
    let text = near.toon(&["route", "list"]);
    assert!(
        text.stdout.contains("g.toon.relay.far -> peer far"),
        "{}",
        text.stdout
    );

    let unrouted = near.toon(&["route", "remove", "g.toon.relay.far", "--json"]);
    assert_eq!(unrouted.exit_code, 0, "{}", unrouted.stdout);
    let unpeered = near.toon(&["peer", "remove", "far", "--json"]);
    assert_eq!(unpeered.exit_code, 0, "{}", unpeered.stdout);

    let routes = near.toon(&["route", "list", "--json"]).json();
    assert_eq!(
        routes["forwarding_routes"].as_array().map(Vec::len),
        Some(0)
    );
    let peers = near.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(0));
}

#[test]
fn every_connector_the_cli_renders_a_config_for_is_peerable() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on_clearnet(&chain).exit_code, 0);

    let config = std::fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/connector.toml"),
    )
    .expect("the connector's config");

    assert!(config.contains("peer_expose = \"http\""), "{config}");
    assert!(
        config.contains("http_endpoint = \"http://127.0.0.1:"),
        "{config}"
    );
    assert!(config.contains("/ilp\""), "{config}");
}

#[test]
fn a_peering_toward_a_connector_that_is_not_peerable_says_the_refusal_is_on_the_other_side() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    // A connector somebody else configured, which exposes no peer carriage.
    let unpeerable = support::unpeerable::start(&chain);

    let peered = near.toon(&[
        "peer",
        "add",
        &unpeerable.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);

    assert_eq!(peered.exit_code, 1, "{}", peered.stdout);
    let error = peered.json()["error"].clone();
    assert_eq!(error["code"], "peer_not_peerable", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("the refusal is on the other side"),
        "{error}"
    );
}

#[test]
fn peer_add_needs_a_deposit() {
    let machine = Machine::new();

    let run = machine.toon(&["peer", "add", "http://127.0.0.1:1/ilp", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn peer_commands_need_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on_clearnet(&chain).exit_code, 0);

    for args in [
        &[
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "1",
            "--yes",
            "--json",
        ][..],
        &["peer", "list", "--json"],
        &["peer", "remove", "far", "--json"],
        &["route", "add", "g.far", "--peer", "far", "--json"],
        &["route", "remove", "g.far", "--json"],
    ] {
        let run = machine.toon(args);
        assert_eq!(run.json()["error"]["code"], "not_running", "{args:?}");
        assert_eq!(run.exit_code, 1);
    }
}

#[test]
fn a_connector_that_dials_no_plaintext_says_the_refusal_is_on_this_side() {
    let chain = AnvilChain::start();
    let near = node_with(&chain, false);
    let far = node_on(&chain);

    let peered = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);

    assert_eq!(peered.exit_code, 1, "{}", peered.stdout);
    let error = peered.json()["error"].clone();
    assert_eq!(error["code"], "peer_failed", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--allow-plaintext-peers"),
        "{error}"
    );
}

#[test]
fn a_label_or_prefix_that_is_not_one_path_segment_is_refused() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on_clearnet(&chain).exit_code, 0);

    for (args, code) in [
        (&["peer", "remove", "far/../x", "--json"][..], "peer_failed"),
        (&["route", "remove", "g.far?x", "--json"], "route_failed"),
    ] {
        let run = machine.toon(args);
        assert_eq!(run.json()["error"]["code"], code, "{args:?}");
        assert_eq!(run.exit_code, 1);
    }
}
