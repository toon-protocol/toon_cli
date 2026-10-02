mod support;

use std::fs;
use std::net::SocketAddr;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::Machine;

const KEY: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const OTHER: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";

/// What the relay was handed besides its key, as its data directory records it.
fn settings(machine: &Machine) -> String {
    fs::read_to_string(machine.agent_node_home().join("apps/relay/data/settings")).unwrap()
}

fn config(machine: &Machine) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/connector.toml"),
    )
    .unwrap()
}

fn relay_address(machine: &Machine) -> SocketAddr {
    machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["apps"][0]["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn config_with_no_changes_shows_the_settings_and_prices() {
    let machine = Machine::new();
    machine.init_with(&[]);

    let run = machine.toon(&["relay", "config", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let shown = run.json();
    assert_eq!(shown["relay"]["name"], Value::Null);
    assert_eq!(shown["relay"]["expiry"], "honour");
    assert_eq!(shown["relay"]["blocklist"], serde_json::json!([]));
    assert_eq!(shown["prices"]["write"], 1);
    assert_eq!(shown["prices"]["ephemeral"], 0);
    assert_eq!(shown["restarted"], false);
    assert_eq!(run.stderr, "");
}

#[test]
fn config_is_recorded_when_no_agent_node_is_running_and_the_relay_gets_it_at_up() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let run = machine.toon(&[
        "relay",
        "config",
        "--name",
        "Corner relay",
        "--description",
        "A small one",
        "--expiry",
        "ignore",
        "--block",
        KEY,
        "--block",
        OTHER,
        "--unblock",
        OTHER,
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["restarted"], false);
    let shown = machine.toon(&["relay", "config", "--json"]).json();
    assert_eq!(shown["relay"]["name"], "Corner relay");
    assert_eq!(shown["relay"]["description"], "A small one");
    assert_eq!(shown["relay"]["expiry"], "ignore");
    assert_eq!(shown["relay"]["blocklist"], serde_json::json!([KEY]));

    machine.start(&["up", "--foreground", "--json"]).report();
    assert_eq!(
        settings(&machine),
        format!(
            "TOON_RELAY_BLOCKLIST={KEY}\nTOON_RELAY_DESCRIPTION=A small one\n\
             TOON_RELAY_EXPIRY=ignore\nTOON_RELAY_NAME=Corner relay\n"
        )
    );
}

#[test]
fn config_restarts_a_running_relay_with_the_new_settings_only_with_yes() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    assert_eq!(settings(&machine), "TOON_RELAY_EXPIRY=honour\n");

    let refused = machine.toon(&["relay", "config", "--name", "Renamed", "--json"]);

    assert_eq!(refused.exit_code, 1);
    assert_eq!(refused.json()["error"]["code"], "confirmation_required");
    assert_eq!(
        machine.toon(&["relay", "config", "--json"]).json()["relay"]["name"],
        Value::Null
    );

    let run = machine.toon(&[
        "relay", "config", "--name", "Renamed", "--block", KEY, "--yes", "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["restarted"], true);
    assert_eq!(
        settings(&machine),
        format!("TOON_RELAY_BLOCKLIST={KEY}\nTOON_RELAY_EXPIRY=honour\nTOON_RELAY_NAME=Renamed\n")
    );
    // The connector routes to the relay that is running now.
    let after = relay_address(&machine);
    assert!(
        config(&machine).contains(&format!("http://{after}/write\"")),
        "{}",
        config(&machine)
    );
    let status = machine.toon(&["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{}", status.stdout);
}

#[test]
fn price_is_recorded_when_no_agent_node_is_running() {
    let machine = Machine::new();
    machine.init_with(&[]);

    let run = machine.toon(&["relay", "price", "25", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["prices"]["write"], 25);
    assert_eq!(
        machine.toon(&["relay", "config", "--json"]).json()["prices"]["write"],
        25
    );
}

#[test]
fn price_restarts_a_running_connector_only_with_yes_and_is_rendered_on_the_write_route() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let refused = machine.toon(&["relay", "price", "7", "--json"]);

    assert_eq!(refused.exit_code, 1);
    assert_eq!(refused.json()["error"]["code"], "confirmation_required");
    assert!(
        config(&machine).contains("price = 1\n"),
        "{}",
        config(&machine)
    );

    let run = machine.toon(&["relay", "price", "7", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["restarted"], true);
    let address = relay_address(&machine);
    let prefix = machine.relay_address();
    assert!(
        config(&machine).contains(&format!(
            "prefix = \"{prefix}\"\nhandler_url = \"http://{address}/write\"\nprice = 7\n"
        )),
        "{}",
        config(&machine)
    );
    assert!(config(&machine).contains(
        "handler_url = \"http://{address}/write-ephemeral\"\nprice = 0\n"
            .replace("{address}", &address.to_string())
            .as_str()
    ));
    let routes = machine.toon(&["route", "list", "--json"]).json();
    let write = routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["prefix"] == machine.relay_address().as_str())
        .expect("the write route");
    assert_eq!(write["price"], 7);
}

#[test]
fn a_blocklist_entry_that_is_not_a_key_is_a_usage_error() {
    let machine = Machine::new();
    machine.init_with(&[]);

    let run = machine.toon(&["relay", "config", "--block", "nope", "--json"]);

    assert_eq!(run.exit_code, 2);
    assert_eq!(run.json()["error"]["code"], "usage");
}

#[test]
fn relay_commands_need_an_agent_node() {
    let machine = Machine::new();

    let run = machine.toon(&["relay", "config", "--json"]);

    assert_eq!(run.exit_code, 3);
    assert_eq!(run.json()["error"]["code"], "no_agent_node");
}
