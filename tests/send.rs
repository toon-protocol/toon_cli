mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// An agent node, running, with the fake relay behind its connector.
struct Running {
    machine: Machine,
    _up: Foreground,
    _chain: FakeChain,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
    }
}

#[test]
fn a_packet_to_the_operators_own_route_is_fulfilled() {
    let node = running();

    let relay = node.machine.relay_prefix();
    let run = node
        .machine
        .toon(&["send", &relay, "--amount", "0", "--yes", "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["status"], 200);
    assert_eq!(report["response"]["body"], "stored");
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
}

#[test]
fn a_packet_to_a_route_under_the_prefix_is_fulfilled() {
    let node = running();

    let write = format!("{}.write", node.machine.relay_prefix());
    let run = node
        .machine
        .toon(&["send", &write, "--amount", "0", "--yes"]);

    assert_eq!(
        run.stdout,
        format!("Fulfilled: 0 base units to {write}. The app answered 200.\n")
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn a_rejected_packet_says_why_and_exits_non_zero() {
    let node = running();

    let run = node
        .machine
        .toon(&["send", "g.nobody.here", "--amount", "0", "--yes", "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], "F02");
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn a_rejected_packet_is_readable_text_without_json() {
    let node = running();

    let run = node
        .machine
        .toon(&["send", "g.nobody.here", "--amount", "0", "--yes"]);

    assert!(
        run.stdout
            .starts_with("Rejected with F02: 0 base units to g.nobody.here."),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
}

#[test]
fn send_needs_an_amount() {
    let machine = Machine::new();

    let run = machine.toon(&["send", "g.toon.abc.relay", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn send_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["send", "g.toon.relay", "--amount", "0", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn send_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["send", "g.toon.relay", "--amount", "0", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn the_connector_lists_the_wallets_operator_write_key() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let key = fs::read(machine.agent_node_home().join("operator.key")).expect("the write key");
    let allowed = fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/operator-write-keys"),
    )
    .expect("the connector's allowlist");
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });

    assert_eq!(key.len(), 32);
    assert_eq!(
        allowed.trim(),
        shown.json()["wallet"]["operator_write_key"],
        "the connector lists the key the wallet shows"
    );
}

#[test]
fn an_agent_node_from_before_the_operator_write_key_still_comes_up() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let home = machine.agent_node_home();
    fs::remove_file(home.join("operator.key")).expect("forget the write key");
    fs::remove_file(home.join("connectors/0/operator-write-keys")).expect("and its allowlist");
    fs::remove_file(home.join("connectors/0/operator-bearer-token")).expect("and the token");

    let up = machine.start(&["up", "--foreground", "--json"]);

    let report = up.report();
    assert!(report.get("error").is_none(), "{report}");
}

#[test]
fn route_list_shows_the_routing_table() {
    let node = running();

    let run = node.machine.toon(&["route", "list", "--json"]);

    let routes = run.json()["routes"].clone();
    let status = node.machine.toon(&["status", "--json"]).json();
    let relay = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
        .as_str()
        .expect("the relay's address")
        .to_owned();
    assert_eq!(routes.as_array().map(Vec::len), Some(2), "{routes}");
    assert_eq!(routes[0]["prefix"], node.machine.relay_prefix());
    assert_eq!(routes[0]["handler_url"], format!("http://{relay}/write"));
    assert_eq!(routes[1]["prefix"], node.machine.ephemeral_prefix());
    assert_eq!(
        routes[1]["handler_url"],
        format!("http://{relay}/write-ephemeral")
    );
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
}

#[test]
fn route_list_is_readable_text_without_json() {
    let node = running();

    let run = node.machine.toon(&["route", "list"]);

    assert!(
        run.stdout.starts_with(&format!(
            "{} -> http://127.0.0.1:",
            node.machine.relay_prefix()
        )),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn route_list_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["route", "list", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}
