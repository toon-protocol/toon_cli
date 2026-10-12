mod support;

use std::fs;

use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// An agent node that is up, with its chain.
fn running() -> (Machine, FakeChain, Foreground) {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, chain, up)
}

/// The `confinement` of the app `name`, as `toon status --json` says.
fn confinement(machine: &Machine, name: &str) -> Value {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|app| app["apps"].as_array().unwrap().iter())
        .find(|app| app["name"] == name)
        .unwrap_or_else(|| panic!("no app {name}: {status}"))["confinement"]
        .clone()
}

fn caller() -> String {
    let id = |flag: &str| {
        let output = std::process::Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    format!("{}:{}", id("-u"), id("-g"))
}

fn defaults(network: &str) -> Value {
    json!({
        "network": network, "user": caller(), "read_only": true,
        "writable": ["/data", "/tmp"], "capabilities": [],
        "memory": 536870912, "cpus": 1.0, "pids": 256, "runtime": null,
    })
}

#[test]
fn status_shows_the_default_confinement_the_relay_and_an_app_at_a_url() {
    let (machine, _chain, _up) = running();
    machine.toon(&["add", "notes", "--to", "relay", "--image", "n:1", "--yes"]);
    machine.toon(&[
        "add",
        "served",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9",
        "--yes",
    ]);

    assert_eq!(confinement(&machine, "notes"), defaults("none"));
    assert_eq!(confinement(&machine, "relay"), defaults("host"));
    assert_eq!(confinement(&machine, "served"), Value::Null);
    let text = machine.toon(&["status"]).stdout;
    assert!(!text.contains("is confined"), "{text}");
}

#[test]
fn each_allowance_shows_in_the_confinement_and_holds_across_down_and_up() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--allow-egress",
        "--scratch",
        "/cache",
        "--scratch",
        "/run/app",
        "--memory",
        "1g",
        "--cpus",
        "0.5",
        "--pids",
        "64",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);

    let expected = json!({
        "network": "egress", "user": caller(), "read_only": true,
        "writable": ["/data", "/tmp", "/cache", "/run/app"], "capabilities": [],
        "memory": 1073741824u64, "cpus": 0.5, "pids": 64, "runtime": null,
    });
    for _ in 0..2 {
        let up = machine.start(&["up", "--foreground", "--json"]);
        up.report();
        assert_eq!(confinement(&machine, "notes"), expected);
        let text = machine.toon(&["status"]).stdout;
        assert!(
            text.contains("notes is confined, and may open connections out"),
            "{text}"
        );
        assert_eq!(machine.toon(&["down", "--json"]).exit_code, 0);
        drop(up);
    }
}

#[test]
fn a_home_written_before_allowances_loads_with_the_defaults() {
    let (machine, _chain, _up) = running();
    machine.toon(&["add", "notes", "--to", "relay", "--image", "n:1", "--yes"]);
    let path = machine.agent_node_home().join("state.json");
    let state: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        state["toon_apps"][0]["apps"][1].get("allow").is_none(),
        "a default app records no allowances"
    );

    assert_eq!(confinement(&machine, "notes"), defaults("none"));
}

#[test]
fn a_value_that_cannot_be_used_and_an_allowance_with_a_url_are_refused_and_change_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let path = machine.agent_node_home().join("state.json");
    let before = fs::read_to_string(&path).unwrap();
    let refused = [
        vec!["--scratch", "cache"],
        vec!["--scratch", "/data"],
        vec!["--scratch", "/tmp"],
        vec!["--memory", "lots"],
        vec!["--memory", "0"],
        vec!["--cpus", "0"],
        vec!["--pids", "0"],
        vec!["--url", "http://127.0.0.1:9", "--allow-egress"],
        vec!["--url", "http://127.0.0.1:9", "--memory", "1g"],
    ];
    for flags in refused {
        let mut args = vec!["add", "notes", "--to", "relay", "--yes", "--json"];
        if !flags.contains(&"--url") {
            args.extend(["--image", "n:1"]);
        }
        args.extend(&flags);

        let run = machine.toon(&args);

        assert_eq!(
            run.json()["error"]["code"],
            "usage",
            "{flags:?}: {}",
            run.stdout
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "{flags:?}");
    }
    let created = machine.toon(&[
        "create",
        "second",
        "--image",
        "n:1",
        "--no-peer",
        "--scratch",
        "/tmp",
        "--json",
    ]);
    assert_eq!(created.json()["error"]["code"], "usage");
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}
