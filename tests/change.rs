mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

fn running() -> (Machine, FakeChain, Foreground) {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, chain, up)
}

fn file(machine: &Machine, name: &str, contents: &[u8]) -> String {
    let path = machine.home().join(name);
    fs::write(&path, contents).unwrap();
    path.display().to_string()
}

/// What the app `name` was last started with, one `NAME=value` per line.
fn environment(machine: &Machine, name: &str) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("apps")
            .join(name)
            .join("data")
            .join("all-environment"),
    )
    .unwrap_or_default()
}

fn app(machine: &Machine, name: &str) -> Value {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|app| app["apps"].as_array().unwrap().iter())
        .find(|app| app["name"] == name)
        .unwrap_or_else(|| panic!("no app {name}: {status}"))
        .clone()
}

fn apps_dir(machine: &Machine, name: &str) -> PathBuf {
    machine.agent_node_home().join("apps").join(name)
}

fn state(machine: &Machine) -> String {
    fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap()
}

fn has(machine: &Machine, name: &str, line: &str) -> bool {
    environment(machine, name).lines().any(|l| l == line)
}

/// An app `notes` with a variable, two secrets and some allowances.
fn with_notes(machine: &Machine) {
    let key = file(machine, "key", b"old-key\n");
    let raw = file(machine, "raw", b"old-raw");
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--price",
        "7",
        "--allow-egress",
        "--scratch",
        "/cache",
        "--memory",
        "1g",
        "--env",
        "A=1",
        "--env",
        "B=2",
        "--secret",
        &format!("KEY={key}"),
        "--secret-file",
        &format!("/run/raw={raw}"),
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_secret_is_replaced_and_everything_else_stays() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = app(&machine, "notes");
    let new = file(&machine, "new", b"new-key\n");

    let run = machine.toon(&[
        "change",
        "notes",
        "--secret",
        &format!("KEY={new}"),
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let report = run.json();
    assert_eq!(report["changed"], "notes");
    assert_eq!(report["toon_app"], "relay");
    assert_eq!(report["restarted"], true);
    assert_eq!(report["set"], json!(["secret KEY"]));
    assert!(!run.stdout.contains("new-key"));
    assert!(
        has(&machine, "notes", "KEY=new-key"),
        "{}",
        environment(&machine, "notes")
    );
    assert!(has(&machine, "notes", "A=1"));
    // The app was started again, so it has another address.
    let unaddressed = |mut value: Value| {
        value.as_object_mut().unwrap().remove("address");
        value.as_object_mut().unwrap().remove("read_address");
        value
    };
    assert_eq!(unaddressed(app(&machine, "notes")), unaddressed(before));
    // No copy of the secret that was replaced is left.
    let dir = apps_dir(&machine, "notes");
    assert!(!dir.join("secrets.before").exists());
    assert!(!dir.join("secrets.new").exists());
    assert_eq!(
        fs::read_to_string(dir.join("secrets/variable-KEY")).unwrap(),
        "new-key"
    );
    assert_eq!(fs::read(dir.join("secrets/file-0")).unwrap(), b"old-raw");
    assert!(!state(&machine).contains("new-key"));
}

#[test]
fn several_changes_apply_with_one_restart() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let restarts = |machine: &Machine| {
        machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["connector"]
            ["restarts"]
            .clone()
    };
    let before = restarts(&machine);

    let run = machine.toon(&[
        "change",
        "notes",
        "--env",
        "C=3",
        "--unset-env",
        "B",
        "--memory",
        "2g",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["unset"], json!(["variable B"]));
    assert_eq!(restarts(&machine), before);
    let notes = app(&machine, "notes");
    assert_eq!(notes["variables"], json!({"A": "1", "C": "3"}));
    assert_eq!(notes["confinement"]["memory"], 2u64 << 30);
    assert_eq!(notes["confinement"]["network"], "egress");
    assert_eq!(notes["price"], 7);
    assert!(has(&machine, "notes", "C=3"));
    assert!(!has(&machine, "notes", "B=2"));
}

#[test]
fn a_limit_goes_back_to_its_default_and_egress_is_taken_away() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);

    let run = machine.toon(&[
        "change",
        "notes",
        "--memory",
        "default",
        "--no-allow-egress",
        "--unset-scratch",
        "/cache/",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let confinement = app(&machine, "notes")["confinement"].clone();
    assert_eq!(confinement["memory"], 536870912);
    assert_eq!(confinement["network"], "none");
    assert_eq!(confinement["writable"], json!(["/data", "/tmp"]));
    let text = machine.toon(&["status"]).stdout;
    assert!(!text.contains("scratch /cache"), "{text}");
}

#[test]
fn a_name_is_one_thing_the_app_is_started_with() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let from = file(&machine, "from", b"from-file\n");

    let run = machine.toon(&[
        "change",
        "notes",
        "--secret",
        &format!("A={from}"),
        "--env",
        "KEY=plain",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let notes = app(&machine, "notes");
    assert_eq!(notes["variables"], json!({"B": "2", "KEY": "plain"}));
    assert_eq!(
        notes["secrets"],
        json!({"variables": ["A"], "files": ["/run/raw"]})
    );
    assert!(has(&machine, "notes", "A=from-file"));
    assert!(has(&machine, "notes", "KEY=plain"));
    let dir = apps_dir(&machine, "notes").join("secrets");
    assert!(dir.join("variable-A").is_file());
    assert!(!dir.join("variable-KEY").exists());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn secrets_taken_away_leave_no_copy() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);

    let run = machine.toon(&[
        "change",
        "notes",
        "--unset-secret",
        "KEY",
        "--unset-secret-file",
        "/run//raw",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        app(&machine, "notes")["secrets"],
        json!({"variables": [], "files": []})
    );
    let dir = apps_dir(&machine, "notes");
    assert!(!dir.join("secrets").exists());
    assert!(!dir.join("secrets.before").exists());
}

#[test]
fn without_yes_it_asks_and_changes_nothing() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = state(&machine);

    let run = machine.toon(&["change", "notes", "--env", "C=3", "--json"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "confirmation_required");
    assert!(run.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("restarts the connector"));
    assert_eq!(state(&machine), before);
}

#[test]
fn with_the_agent_node_stopped_the_change_waits_for_the_next_up() {
    let (machine, _chain, up) = running();
    with_notes(&machine);
    let new = file(&machine, "new", b"new-key\n");
    assert_eq!(machine.toon(&["down", "--json"]).exit_code, 0);
    drop(up);

    let run = machine.toon(&[
        "change",
        "notes",
        "--secret",
        &format!("KEY={new}"),
        "--env",
        "C=3",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], false);
    assert!(!apps_dir(&machine, "notes").join("secrets.before").exists());
    let _again = machine.start(&["up", "--foreground", "--json"]);
    _again.report();
    assert!(has(&machine, "notes", "KEY=new-key"));
    assert!(has(&machine, "notes", "C=3"));
}

#[test]
fn a_change_the_app_does_not_start_with_is_put_back() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = state(&machine);
    let new = file(&machine, "new", b"new-key\n");

    let run = machine.toon(&[
        "change",
        "notes",
        "--secret",
        &format!("KEY={new}"),
        "--env",
        "FAKE_APP_FAILS=1",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "app_failed");
    for output in [&run.stdout, &run.stderr] {
        assert!(!output.contains("new-key"), "{output}");
    }
    assert_eq!(state(&machine), before);
    // The earlier copy is where it was, and the new one is gone.
    let dir = apps_dir(&machine, "notes");
    assert_eq!(
        fs::read_to_string(dir.join("secrets/variable-KEY")).unwrap(),
        "old-key"
    );
    assert!(!dir.join("secrets.before").exists());
    assert!(!dir.join("secrets.new").exists());
    // The app runs as it did, and its connector runs.
    assert!(has(&machine, "notes", "KEY=old-key"));
    assert!(!has(&machine, "notes", "FAKE_APP_FAILS=1"));
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["connector"]["running"],
        true
    );
    assert_eq!(app(&machine, "notes")["running"], true);
}

#[test]
fn what_cannot_be_changed_is_refused_and_nothing_is_recorded() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    machine.toon(&[
        "add",
        "served",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9",
        "--yes",
    ]);
    let good = file(&machine, "good", b"v\n");
    let before = state(&machine);
    let cases: Vec<Vec<String>> = vec![
        // no flag
        vec!["notes".into()],
        // no difference
        vec!["notes".into(), "--env".into(), "A=1".into()],
        vec!["notes".into(), "--allow-egress".into()],
        vec!["notes".into(), "--memory".into(), "1g".into()],
        vec!["notes".into(), "--cpus".into(), "default".into()],
        // taking away what is not there
        vec!["notes".into(), "--unset-env".into(), "NOPE".into()],
        vec!["notes".into(), "--unset-env".into(), "KEY".into()],
        vec!["notes".into(), "--unset-secret".into(), "A".into()],
        vec!["notes".into(), "--unset-secret-file".into(), "/nope".into()],
        vec!["notes".into(), "--unset-scratch".into(), "/nope".into()],
        // a name twice
        vec![
            "notes".into(),
            "--env".into(),
            "C=1".into(),
            "--unset-env".into(),
            "C".into(),
        ],
        vec![
            "notes".into(),
            "--env".into(),
            "C=1".into(),
            "--secret".into(),
            format!("C={good}"),
        ],
        // across the flags and the app
        vec![
            "notes".into(),
            "--secret-file".into(),
            format!("/cache/k={good}"),
        ],
        vec!["notes".into(), "--scratch".into(), "/run/raw".into()],
        vec!["notes".into(), "--env".into(), "TOON_X=1".into()],
        // an app at a URL
        vec!["served".into(), "--env".into(), "A=1".into()],
        vec!["served".into(), "--memory".into(), "1g".into()],
        // the relay takes limits and nothing else
        vec!["relay".into(), "--env".into(), "A=1".into()],
        vec!["relay".into(), "--scratch".into(), "/x".into()],
        vec!["relay".into(), "--allow-egress".into()],
        vec!["relay".into(), "--unset-env".into(), "A".into()],
        vec!["relay".into(), "--secret".into(), format!("A={good}")],
    ];
    for case in cases {
        let mut args = vec!["change".to_owned()];
        args.extend(case.iter().cloned());
        args.extend(["--yes".into(), "--json".into()]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = machine.toon(&args);
        assert_eq!(run.exit_code, 2, "{case:?}: {}{}", run.stdout, run.stderr);
        assert_eq!(run.json()["error"]["code"], "usage", "{case:?}");
        assert_eq!(state(&machine), before, "{case:?}");
    }
    let dir = apps_dir(&machine, "notes");
    assert_eq!(fs::read_dir(dir.join("secrets")).unwrap().count(), 2);
    assert!(!dir.join("secrets.new").exists());
}

#[test]
fn the_message_names_what_the_app_has() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);

    let run = machine.toon(&["change", "notes", "--unset-env", "NOPE", "--yes", "--json"]);

    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("A, B"), "{message}");
}

#[test]
fn an_unknown_app_is_unknown_name() {
    let (machine, _chain, _up) = running();

    let run = machine.toon(&["change", "ghost", "--env", "A=1", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "unknown_name");
}

#[test]
fn the_relay_is_given_a_limit() {
    let (machine, _chain, _up) = running();

    let run = machine.toon(&["change", "relay", "--memory", "1g", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(app(&machine, "relay")["confinement"]["memory"], 1u64 << 30);
    assert_eq!(app(&machine, "relay")["running"], true);
}

#[test]
fn help_lists_change() {
    let machine = Machine::new();
    let help = machine.toon(&["--help"]);
    assert!(help.stdout.contains("change"), "{}", help.stdout);
}
