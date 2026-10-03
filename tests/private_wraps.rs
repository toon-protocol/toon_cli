//! The agent node's own relay serves a gift wrap only to the key it is addressed to
//! (ADR 0008), and the relay is always started so.

mod support;

use std::fs;

use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::Machine;

fn relay_url(machine: &Machine) -> String {
    let status = machine.toon(&["status", "--json"]).json();
    let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
        .as_str()
        .unwrap_or_else(|| panic!("the relay has no address: {status}"));
    format!("ws://{address}")
}

fn names(machine: &Machine) -> String {
    fs::read_to_string(machine.agent_node_home().join("apps/relay/data/names")).unwrap()
}

fn query(machine: &Machine, filter: &Value) -> support::Run {
    machine.toon(&[
        "event",
        "query",
        &relay_url(machine),
        "--filter",
        &filter.to_string(),
        "--json",
    ])
}

#[test]
fn the_relay_is_started_with_recipient_only_wraps_with_and_without_a_sold_feed() {
    for sells in [false, true] {
        let chain = FakeChain::start();
        let machine = Machine::new();
        machine.init_on(&chain);
        if sells {
            let run = machine.toon(&[
                "relay",
                "price",
                "--subscribe",
                "1000",
                "--broadcast",
                "10",
                "--json",
            ]);
            assert_eq!(run.exit_code, 0, "{}", run.stdout);
        }
        let _up = machine.start(&["up", "--foreground", "--json"]);
        _up.report();
        let names = names(&machine);
        assert!(
            names
                .lines()
                .any(|name| name == "TOON_NIP17_RECIPIENT_ONLY"),
            "{names}"
        );
        // The agent identity is never made an operator to read its wraps.
        assert!(
            !names.lines().any(|name| name == "TOON_OPERATOR_PUBKEYS"),
            "{names}"
        );
    }
}

#[test]
fn a_query_for_wraps_is_refused_and_one_for_notes_is_not() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let wraps = query(&machine, &json!({ "kinds": [1059] }));
    assert_ne!(wraps.exit_code, 0, "{}", wraps.stdout);
    assert!(
        format!("{}{}", wraps.stdout, wraps.stderr).contains("auth-required:"),
        "{}{}",
        wraps.stdout,
        wraps.stderr
    );

    let notes = query(&machine, &json!({ "kinds": [1] }));
    assert_eq!(notes.exit_code, 0, "{}{}", notes.stdout, notes.stderr);
}
