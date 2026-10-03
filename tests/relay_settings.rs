mod support;

use std::fs;
use std::net::SocketAddr;

use serde_json::{json, Value};
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
    assert!(
        config(&machine).contains(&format!(
            "prefix = \"{}\"\nhandler_url = \"http://{address}/write\"\nprice = 7\n",
            machine.relay_prefix()
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
        .find(|route| route["prefix"] == machine.relay_prefix().as_str())
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

fn connector_env(machine: &Machine) -> String {
    fs::read_to_string(machine.agent_node_home().join("apps/relay/data/connector")).unwrap()
}

#[test]
fn selling_the_feed_renders_a_subscribe_route_and_hands_the_relay_its_settings() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    assert!(
        !config(&machine).contains(".subscribe"),
        "{}",
        config(&machine)
    );
    assert!(!connector_env(&machine).contains("TOON_SUBSCRIBE_ILP_ADDRESS"));

    let refused = machine.toon(&[
        "relay",
        "price",
        "--subscribe",
        "1000",
        "--broadcast",
        "10",
        "--json",
    ]);
    assert_eq!(refused.json()["error"]["code"], "confirmation_required");

    let run = machine.toon(&[
        "relay",
        "price",
        "--subscribe",
        "1000",
        "--broadcast",
        "10",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let shown = run.json();
    assert_eq!(shown["restarted"], true);
    assert_eq!(shown["prices"]["subscribe"], 1000);
    assert_eq!(shown["prices"]["broadcast"], 10);
    assert_eq!(shown["prices"]["write"], 1);
    let address = relay_address(&machine);
    assert!(
        config(&machine).contains(&format!(
            "prefix = \"{}.subscribe\"\nhandler_url = \"http://{address}/subscribe\"\nprice = 1000\n",
            machine.relay_prefix()
        )),
        "{}",
        config(&machine)
    );
    let env = connector_env(&machine);
    assert!(
        env.contains(&format!(
            "TOON_SUBSCRIBE_ILP_ADDRESS={}.subscribe\n",
            machine.relay_prefix()
        )),
        "{env}"
    );
    assert!(env.contains("TOON_BROADCAST_PRICE=10\n"), "{env}");
    assert!(env.contains("TOON_RELAY_URL=ws://"), "{env}");
    let routes = machine.toon(&["route", "list", "--json"]).json();
    let subscribe = routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["prefix"] == format!("{}.subscribe", machine.relay_prefix()).as_str())
        .expect("the subscribe route");
    assert_eq!(subscribe["price"], 1000);

    // The write price is not touched by it, and a price of 0 stops selling.
    let stopped = machine.toon(&["relay", "price", "--subscribe", "0", "--yes", "--json"]);
    assert_eq!(stopped.exit_code, 0, "{}", stopped.stdout);
    assert_eq!(stopped.json()["prices"]["subscribe"], Value::Null);
    assert!(
        !config(&machine).contains(".subscribe"),
        "{}",
        config(&machine)
    );
    assert!(!connector_env(&machine).contains("TOON_BROADCAST_PRICE"));
}

#[test]
fn the_feed_is_sold_at_two_prices_together() {
    let machine = Machine::new();
    machine.init_with(&[]);

    let alone = machine.toon(&["relay", "price", "--subscribe", "1000", "--json"]);
    assert_eq!(alone.exit_code, 2, "{}", alone.stdout);
    assert_eq!(alone.json()["error"]["code"], "usage");

    let nothing = machine.toon(&["relay", "price", "--json"]);
    assert_eq!(nothing.exit_code, 2, "{}", nothing.stdout);

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
    let shown = machine.toon(&["relay", "config", "--json"]).json();
    assert_eq!(shown["prices"]["subscribe"], 1000);
    assert_eq!(shown["prices"]["broadcast"], 10);
}

#[test]
fn incoming_subscriptions_are_the_relays_list_of_subscribers() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let unsold = machine.toon(&["relay", "subscriptions", "--incoming", "--json"]);
    assert_eq!(unsold.exit_code, 1, "{}", unsold.stdout);
    assert_eq!(unsold.json()["error"]["code"], "query_failed");

    machine
        .toon(&[
            "relay",
            "price",
            "--subscribe",
            "1000",
            "--broadcast",
            "10",
            "--json",
        ])
        .json();
    let down = machine.toon(&["relay", "subscriptions", "--incoming", "--json"]);
    assert_eq!(down.json()["error"]["code"], "not_running");

    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    let listed = r#"{"broadcast_price":10,"subscribers":[{"pubkey":"7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e","balance":990,"broadcast_price":10,"filter":{"kinds":[1]}},{"pubkey":"0f0f9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e","balance":0,"broadcast_price":10,"filter":{"kinds":[1]}}]}"#;
    fs::write(
        machine
            .agent_node_home()
            .join("apps/relay/data/subscribers.json"),
        listed,
    )
    .unwrap();

    let run = machine.toon(&["relay", "subscriptions", "--incoming", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let shown = run.json();
    assert_eq!(shown["broadcast_price"], 10);
    assert_eq!(shown["subscribers"][0]["balance"], 990);
    assert_eq!(
        shown["subscribers"][0]["pubkey"],
        "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"
    );
    assert_eq!(shown["totals"], json!({ "subscribers": 1 }));
    let text = machine.toon(&["relay", "subscriptions", "--incoming"]);
    assert!(text.stdout.contains("Subscriber keys with a balance: 1."));
    assert!(!text.stdout.contains("peer"));

    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["totals"],
        json!({ "subscriptions": { "active": 0, "exhausted": 0 }, "subscribers": 1 })
    );
    let status = machine.toon(&["status"]);
    assert!(status
        .stdout
        .contains("Subscriber keys of its own relay with a balance: 1."));
}

#[test]
fn status_counts_both_directions_and_says_unknown_while_the_relay_is_stopped() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    // A relay that sells no live feed has no subscriber, running or not.
    let unsold = machine.toon(&["status", "--json"]);
    assert_eq!(unsold.json()["agent_node"]["totals"]["subscribers"], 0);
    machine
        .toon(&[
            "relay",
            "price",
            "--subscribe",
            "1000",
            "--broadcast",
            "10",
            "--json",
        ])
        .json();

    let stopped = machine.toon(&["status", "--json"]);
    assert_eq!(stopped.exit_code, 1, "{}", stopped.stdout);
    assert_eq!(
        stopped.json()["agent_node"]["totals"],
        json!({
            "subscriptions": { "active": 0, "exhausted": 0 },
            "subscribers": null,
        })
    );
    let text = machine.toon(&["status"]);
    assert_eq!(text.exit_code, 1);
    assert!(text
        .stdout
        .contains("Subscriptions held: 0 with a balance, 0 exhausted."));
    assert!(text.stdout.contains("with a balance: unknown."));

    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    fs::write(
        machine
            .agent_node_home()
            .join("apps/relay/data/subscribers.json"),
        r#"{"broadcast_price":10,"subscribers":[]}"#,
    )
    .unwrap();
    let running = machine.toon(&["status", "--json"]);
    assert_eq!(running.exit_code, 0, "{}", running.stdout);
    assert_eq!(running.json()["agent_node"]["totals"]["subscribers"], 0);
}
