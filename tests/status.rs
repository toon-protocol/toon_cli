mod support;

use serde_json::json;
use support::Machine;

#[test]
fn status_says_there_is_no_agent_node_on_a_new_machine() {
    let machine = Machine::new();

    let run = machine.toon(&["status", "--json"]);

    assert_eq!(
        run.json(),
        json!({
            "home": machine.agent_node_home(),
            "agent_node": null,
        })
    );
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.stderr, "");
}

#[test]
fn status_is_readable_text_without_json() {
    let machine = Machine::new();

    let run = machine.toon(&["status"]);

    assert_eq!(
        run.stdout,
        format!(
            "No agent node at {}.\n",
            machine.agent_node_home().display()
        )
    );
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.stderr, "");
}

#[test]
fn status_reads_peerings_as_unknown_while_the_supervisor_is_down() {
    let machine = Machine::new();
    let chain = support::fake_chain::FakeChain::start();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["status", "--json"]);
    assert_eq!(run.exit_code, 1);
    assert!(run.json()["agent_node"]["peerings"].is_null());
    let text = machine.toon(&["status"]).stdout;
    assert!(!text.contains("Unconnected"), "{text}");
    assert!(text.contains("No network joined."), "{text}");
}
