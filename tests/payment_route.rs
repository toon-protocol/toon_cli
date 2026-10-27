mod support;

use std::fs;
use std::process::Stdio;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::Machine;

/// ADR 0083: the embedded connector accepts `payment = true` on a route that ends at an
/// app (`handler_url`), which is the key the revision before it refused.
#[test]
fn the_embedded_connector_accepts_a_payment_route_with_a_handler_url() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let mut up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    assert_eq!(machine.toon(&["down"]).exit_code, 0);
    assert_eq!(up.exit_code(), 0);

    let path = machine
        .agent_node_home()
        .join("connectors/0/connector.toml");
    let config = fs::read_to_string(&path).unwrap();
    let route = format!(
        "prefix = \"{}\"\nhandler_url = ",
        machine.ephemeral_prefix()
    );
    assert!(config.contains(&route), "{config}");
    let marked = config.replacen(&route, &format!("payment = true\n{route}"), 1);
    fs::write(&path, marked).unwrap();

    let connector = machine.start_with(&["connector", path.to_str().unwrap()], |command| {
        command.stdin(Stdio::piped());
    });
    let said: Value = serde_json::from_str(&connector.line()).unwrap();

    assert!(
        said["listening"].is_string(),
        "{said}\n{}",
        connector.stderr()
    );
}
