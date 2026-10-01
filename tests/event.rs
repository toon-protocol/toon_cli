mod support;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// An agent node, running, with the fake relay behind its connector.
struct Running {
    machine: Machine,
    up: Foreground,
    _chain: FakeChain,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    Running {
        machine,
        up,
        _chain: chain,
    }
}

impl Running {
    fn publish(&self, args: &[&str]) -> support::Run {
        let mut all = vec!["event", "publish", "--json"];
        all.extend_from_slice(args);
        self.machine.toon_with(&all, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    /// The websocket URL of the relay: the fake serves it on its write port.
    fn relay_url(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"));
        format!("ws://{address}")
    }

    fn agent_identity(&self) -> String {
        let show = self
            .machine
            .toon_with(&["wallet", "show", "--json"], |command| {
                command.env("TOON_PASSPHRASE", support::PASSPHRASE);
            });
        show.json()["wallet"]["agent_identity"]
            .as_str()
            .expect("the agent identity")
            .to_owned()
    }
}

#[test]
fn a_published_event_is_read_back_from_the_relay() {
    let node = running();
    let _ = &node.up;

    let published = node.publish(&[
        "--kind",
        "1",
        "--content",
        "hello toon",
        "--tags",
        r#"[["t","toon"]]"#,
    ]);

    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    let event = report["event"].clone();
    assert_eq!(event["pubkey"], node.agent_identity());
    assert_eq!(event["kind"], 1);
    assert_eq!(event["content"], "hello toon");
    assert_eq!(event["tags"][0][1], "toon");

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
        "--json",
    ]);

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    assert_eq!(query.stderr, "");
    let events = query.json()["events"].clone();
    assert_eq!(events, Value::Array(vec![event]));
}

#[test]
fn a_query_returns_only_the_events_the_filter_matches() {
    let node = running();
    assert_eq!(
        node.publish(&["--kind", "1", "--content", "a"]).exit_code,
        0
    );
    assert_eq!(
        node.publish(&["--kind", "7", "--content", "b"]).exit_code,
        0
    );

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[7]}"#,
        "--json",
    ]);

    let events = query.json()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    assert_eq!(events[0]["content"], "b");
}

#[test]
fn a_query_that_matches_nothing_says_so() {
    let node = running();

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
    ]);

    assert_eq!(query.stdout, "No stored event matches.\n");
    assert_eq!(query.exit_code, 0);
}

#[test]
fn publish_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon_with(&["event", "publish", "--kind", "1", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn publish_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "publish", "--kind", "1", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn publish_refuses_tags_that_are_not_arrays_of_strings() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "publish", "--kind", "1", "--tags", "[1]", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn query_refuses_a_filter_that_is_not_an_object() {
    let machine = Machine::new();

    let run = machine.toon(&[
        "event",
        "query",
        "ws://127.0.0.1:1",
        "--filter",
        "[]",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn query_of_a_relay_that_is_not_there_fails() {
    let machine = Machine::new();

    let run = machine.toon(&[
        "event",
        "query",
        "ws://127.0.0.1:1",
        "--filter",
        "{}",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "query_failed");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn query_of_a_relay_over_tls_is_refused_plainly() {
    let machine = Machine::new();

    let run = machine.toon(&[
        "event",
        "query",
        "wss://relay.example",
        "--filter",
        "{}",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "query_failed");
    assert!(run.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ws://"));
}
