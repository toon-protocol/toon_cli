mod support;

use std::fs;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::Machine;

/// What the relay was told of its connector, as its data directory records it.
fn handed(machine: &Machine) -> Vec<(String, String)> {
    fs::read_to_string(machine.agent_node_home().join("apps/relay/data/connector"))
        .unwrap()
        .lines()
        .map(|line| {
            let (name, value) = line.split_once('=').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn get(handed: &[(String, String)], name: &str) -> String {
    handed
        .iter()
        .find(|(candidate, _)| candidate == name)
        .unwrap_or_else(|| panic!("{name} was not handed: {handed:?}"))
        .1
        .clone()
}

fn describe(url: &str) -> Value {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(url)
        .send()
        .unwrap()
        .json()
        .expect("a self-description")
}

#[test]
fn the_relay_is_handed_its_connector_and_its_own_prefix() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let handed = handed(&machine);

    let url = get(&handed, "TOON_CONNECTOR_URL");
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(url.ends_with("/ilp"), "{url}");
    let prefix = get(&handed, "TOON_WRITE_ILP_ADDRESS");
    assert_eq!(prefix, "g.toon.relay");
    let description = describe(&url);
    assert!(
        description["routes"].as_array().is_some_and(|routes| routes
            .iter()
            .any(|route| route["prefix"] == prefix.as_str())),
        "{description}"
    );
}

#[test]
fn a_relay_whose_route_has_another_prefix_is_handed_that_prefix() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let path = machine.agent_node_home().join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["toon_apps"][0]["apps"][0] =
        serde_json::json!({ "name": "relay", "price": 1, "prefix": "g.toon.elsewhere" });
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let handed = handed(&machine);

    assert_eq!(get(&handed, "TOON_WRITE_ILP_ADDRESS"), "g.toon.elsewhere");
    let description = describe(&get(&handed, "TOON_CONNECTOR_URL"));
    assert!(description["routes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|route| route["prefix"] == "g.toon.elsewhere"));
}

#[test]
fn a_hidden_toon_apps_relay_is_handed_the_loopback_url_and_the_connector_names_the_onion() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let url = get(&handed(&machine), "TOON_CONNECTOR_URL");

    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let description = describe(&url);
    assert!(
        description["httpEndpoint"]
            .as_str()
            .is_some_and(|endpoint| endpoint.contains(".anyone")),
        "{description}"
    );
}

#[test]
fn status_names_where_the_relay_is_read_on_this_machine() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let status = machine.toon(&["status", "--json"]).json();

    let relay = &status["agent_node"]["toon_apps"][0]["apps"][0];
    let read: std::net::SocketAddr = relay["read_address"].as_str().unwrap().parse().unwrap();
    assert!(read.ip().is_loopback());
    assert_ne!(relay["read_address"], relay["address"]);
}
