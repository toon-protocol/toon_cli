mod support;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// An agent node on the fake chain, running.
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
fn a_connector_that_has_carried_nothing_counts_zero_without_a_passphrase() {
    let node = running();

    let run = node.machine.toon(&["packet", "count", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(
        run.json(),
        serde_json::json!({
            "toon_app": "relay",
            "packets": { "fulfilled": 0, "rejected": 0 },
            "rejects": {},
            "fees_earned": "0",
        })
    );
    let text = node.machine.toon(&["packet", "count"]);
    assert_eq!(text.exit_code, 0);
    assert!(
        text.stdout.contains("Packets fulfilled: 0"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains("Packets rejected: 0"),
        "{}",
        text.stdout
    );
    assert!(text.stdout.contains("Fees earned: 0"), "{}", text.stdout);
    assert!(text.stdout.contains("last started"), "{}", text.stdout);
}

#[test]
fn a_rejected_packet_is_counted_by_its_code() {
    let node = running();
    let sent = node
        .machine
        .toon(&["send", "g.nobody.here", "--amount", "0", "--yes", "--json"]);
    assert_eq!(sent.json()["reject"]["code"], "F02", "{}", sent.stdout);

    let run = node.machine.toon(&["packet", "count", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let report = run.json();
    assert_eq!(report["packets"]["rejected"], 1, "{report}");
    assert_eq!(
        report["rejects"],
        serde_json::json!({ "F02": 1 }),
        "{report}"
    );
    let text = node.machine.toon(&["packet", "count"]);
    assert!(text.stdout.contains("  F02: 1"), "{}", text.stdout);
}

#[test]
fn an_unknown_name_is_refused() {
    let node = running();

    let run = node
        .machine
        .toon(&["packet", "count", "--app", "nobody", "--json"]);

    assert_ne!(run.exit_code, 0);
    assert_eq!(
        run.json()["error"]["code"],
        "unknown_name",
        "{}",
        run.stdout
    );
}

#[test]
fn packet_count_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["packet", "count", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn packet_count_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["packet", "count", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn the_help_says_no_passphrase_is_needed() {
    let machine = Machine::new();
    for args in [&["packet", "--help"][..], &["packet", "count", "--help"]] {
        let run = machine.toon(args);
        assert_eq!(run.exit_code, 0);
        assert!(
            run.stdout.contains("needs no passphrase"),
            "{args:?}: {}",
            run.stdout
        );
    }
}

fn reject(node: &Running, address: &str) {
    let sent = node
        .machine
        .toon(&["send", address, "--amount", "0", "--yes", "--json"]);
    assert_eq!(sent.json()["reject"]["code"], "F02", "{}", sent.stdout);
}

#[test]
fn a_connector_that_rejected_nothing_lists_nothing() {
    let node = running();

    let run = node.machine.toon(&["packet", "list", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(
        run.json(),
        serde_json::json!({ "toon_app": "relay", "packets": [] })
    );
    let text = node.machine.toon(&["packet", "list"]);
    assert_eq!(text.exit_code, 0);
    assert!(
        text.stdout.contains("No packet was rejected"),
        "{}",
        text.stdout
    );
    assert!(text.stdout.contains("toon packet count"), "{}", text.stdout);
}

#[test]
fn rejected_packets_are_listed_newest_first_and_limited() {
    let node = running();
    reject(&node, "g.nobody.first");
    reject(&node, "g.nobody.second");

    let run = node.machine.toon(&["packet", "list", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let report = run.json();
    let packets = report["packets"].as_array().expect("packets");
    assert_eq!(packets.len(), 2, "{report}");
    assert_eq!(packets[0]["destination"], "g.nobody.second");
    assert_eq!(packets[1]["destination"], "g.nobody.first");
    assert_eq!(packets[0]["outcome"], "rejected");
    assert_eq!(packets[0]["code"], "F02");
    assert!(packets[0]["message"]
        .as_str()
        .is_some_and(|m| !m.is_empty()));
    assert!(packets[0]["time"].as_str().is_some());
    let text = node.machine.toon(&["packet", "list"]);
    assert!(text.stdout.contains("g.nobody.second"), "{}", text.stdout);
    assert!(text.stdout.contains("toon packet count"), "{}", text.stdout);

    let one = node.machine.toon(&["packet", "list", "-n", "1", "--json"]);
    let one = one.json();
    assert_eq!(one["packets"].as_array().expect("packets").len(), 1);
    assert_eq!(one["packets"][0]["destination"], "g.nobody.second");
}

#[test]
fn rejects_are_listed_while_the_agent_node_is_stopped() {
    let node = running();
    reject(&node, "g.nobody.here");
    assert_eq!(node.machine.toon(&["down"]).exit_code, 0);

    let run = node.machine.toon(&["packet", "list", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["packets"][0]["destination"], "g.nobody.here");
}

#[test]
fn packet_list_names_a_toon_app_with_app() {
    let node = running();
    reject(&node, "g.nobody.here");

    let run = node
        .machine
        .toon(&["--app", "relay", "packet", "list", "--json"]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["packets"][0]["destination"], "g.nobody.here");
    let unknown = node
        .machine
        .toon(&["packet", "list", "--app", "nobody", "--json"]);
    assert_eq!(unknown.json()["error"]["code"], "unknown_name");
    assert_ne!(unknown.exit_code, 0);
}

#[test]
fn packet_list_without_an_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["packet", "list", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn the_list_help_says_rejected_only_and_no_passphrase() {
    let run = Machine::new().toon(&["packet", "list", "--help"]);
    assert_eq!(run.exit_code, 0);
    assert!(run.stdout.contains("needs no passphrase"), "{}", run.stdout);
    assert!(
        run.stdout.contains("only rejected packets"),
        "{}",
        run.stdout
    );
}
