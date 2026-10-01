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
