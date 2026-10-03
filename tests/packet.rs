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
            "fees_earned": 0,
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
