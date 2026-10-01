mod support;

use support::Machine;

#[test]
fn an_error_with_json_is_one_json_document_with_a_stable_code() {
    let machine = Machine::new();

    let run = machine.toon_with(&["status", "--json"], |command| {
        command.env_remove("HOME");
    });

    let document = run.json();
    assert_eq!(document["error"]["code"], "home_unresolved");
    assert!(document["error"]["message"].is_string());
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn an_error_without_json_is_text_on_stderr() {
    let machine = Machine::new();

    let run = machine.toon_with(&["status"], |command| {
        command.env_remove("HOME");
    });

    assert!(run.stderr.starts_with("error: "), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn an_unknown_command_with_json_is_a_usage_error() {
    let machine = Machine::new();

    let run = machine.toon(&["frobnicate", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
    assert_eq!(run.stderr, "");
}

#[test]
fn an_unknown_command_without_json_is_text_on_stderr() {
    let machine = Machine::new();

    let run = machine.toon(&["frobnicate"]);

    assert!(run.stderr.contains("frobnicate"), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn no_command_at_all_is_a_usage_error() {
    let machine = Machine::new();

    let run = machine.toon(&["--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn json_is_accepted_before_the_command_as_well_as_after() {
    let machine = Machine::new();

    let before = machine.toon(&["--json", "status"]);
    let after = machine.toon(&["status", "--json"]);

    assert_eq!(before.json(), after.json());
    assert_eq!(before.exit_code, after.exit_code);
}

#[test]
fn help_is_a_flag_and_not_a_command() {
    let machine = Machine::new();

    for args in [["help", "--json"], ["--json", "help"]] {
        let run = machine.toon(&args);

        assert_eq!(run.json()["error"]["code"], "usage");
        assert_eq!(run.exit_code, 2);
    }
}

#[test]
fn json_given_a_value_is_still_a_usage_error_in_json() {
    let machine = Machine::new();

    let run = machine.toon(&["status", "--json=true"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
    assert_eq!(run.stderr, "");
}

#[test]
fn output_that_cannot_be_written_is_a_failure() {
    let machine = Machine::new();

    let run = machine.toon_with(&["status", "--json"], |command| {
        command.stdout(std::fs::File::create("/dev/full").expect("open /dev/full"));
    });

    assert_eq!(run.exit_code, 1);
}
