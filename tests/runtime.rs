mod support;

use std::fs;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

const OFF_LIST: &str = "docker.io/someone/else:1";
const ON_LIST: &str = "ghcr.io/toon-protocol/echo:1";

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

fn runtime(machine: &Machine, name: &str) -> Value {
    confinement(machine, name)["runtime"].clone()
}

fn add(machine: &Machine, name: &str, image: &str, extra: &[&str]) -> support::Run {
    let mut args = vec![
        "add", name, "--to", "relay", "--image", image, "--yes", "--json",
    ];
    args.extend_from_slice(extra);
    machine.toon(&args)
}

fn error_code(run: &support::Run) -> String {
    run.json()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_runtime_for_one_app_shows_in_the_confinement_and_holds_across_down_and_up() {
    let (machine, _chain, up) = running();
    let added = add(&machine, "boxed", OFF_LIST, &["--runtime", "runsc"]);
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    assert_eq!(add(&machine, "plain", OFF_LIST, &[]).exit_code, 0);

    assert_eq!(runtime(&machine, "boxed"), "runsc");
    assert_eq!(runtime(&machine, "plain"), Value::Null);
    assert_eq!(runtime(&machine, "relay"), Value::Null);
    let text = machine.toon(&["status"]).stdout;
    assert!(
        text.contains("boxed runs under the runtime runsc"),
        "{text}"
    );
    assert!(
        !text.contains("boxed is confined"),
        "a runtime is no allowance: {text}"
    );

    assert_eq!(machine.toon(&["down", "--json"]).exit_code, 0);
    drop(up);
    let _up = machine.start(&["up", "--foreground", "--json"]);
    assert_eq!(runtime(&machine, "boxed"), "runsc");
    assert_eq!(runtime(&machine, "plain"), Value::Null);
}

#[test]
fn the_nodes_runtime_applies_to_an_image_off_the_list_unless_default_is_named() {
    let (machine, _chain, _up) = running();
    machine.write_agent_node_file("untrusted-runtime", "# gVisor\n\nrunsc\nkata\n");

    assert_eq!(add(&machine, "off", OFF_LIST, &[]).exit_code, 0);
    assert_eq!(add(&machine, "on", ON_LIST, &[]).exit_code, 0);
    assert_eq!(
        add(&machine, "plain", OFF_LIST, &["--runtime", "default"]).exit_code,
        0
    );
    assert_eq!(
        add(&machine, "named", ON_LIST, &["--runtime", "kata"]).exit_code,
        0
    );
    assert_eq!(runtime(&machine, "off"), "runsc");
    assert_eq!(runtime(&machine, "on"), Value::Null);
    assert_eq!(runtime(&machine, "plain"), Value::Null);
    assert_eq!(runtime(&machine, "named"), "kata");

    // The runtime was settled when the apps were added.
    fs::write(
        machine.agent_node_home().join("untrusted-runtime"),
        "other\n",
    )
    .unwrap();
    assert_eq!(runtime(&machine, "off"), "runsc");
    fs::remove_file(machine.agent_node_home().join("untrusted-runtime")).unwrap();
    fs::write(machine.agent_node_home().join("trusted-images"), "").unwrap();
    assert_eq!(runtime(&machine, "off"), "runsc");
    assert_eq!(runtime(&machine, "on"), Value::Null);

    // A node with no runtime gives none.
    assert_eq!(add(&machine, "later", OFF_LIST, &[]).exit_code, 0);
    assert_eq!(runtime(&machine, "later"), Value::Null);
}

#[test]
fn change_sets_and_clears_the_runtime_and_keeps_what_else_the_app_has() {
    let (machine, _chain, _up) = running();
    let added = add(
        &machine,
        "boxed",
        OFF_LIST,
        &["--allow-egress", "--memory", "1g", "--env", "A=b"],
    );
    assert_eq!(added.exit_code, 0, "{}", added.stdout);
    let before = confinement(&machine, "boxed");

    let set = machine.toon(&["change", "boxed", "--runtime", "runsc", "--yes", "--json"]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);
    let after = confinement(&machine, "boxed");
    assert_eq!(after["runtime"], "runsc");
    assert_eq!(after["network"], before["network"]);
    assert_eq!(after["memory"], before["memory"]);

    let again = machine.toon(&["change", "boxed", "--runtime", "runsc", "--yes", "--json"]);
    assert_eq!(error_code(&again), "usage");

    let cleared = machine.toon(&["change", "boxed", "--runtime", "default", "--yes", "--json"]);
    assert_eq!(cleared.exit_code, 0, "{}{}", cleared.stdout, cleared.stderr);
    assert_eq!(confinement(&machine, "boxed"), before);
}

#[test]
fn an_unusable_name_a_url_and_the_relay_are_refused_and_change_nothing() {
    let (machine, _chain, _up) = running();
    for name in ["", "-x", "--rm", "a b", "a/b", "a;b"] {
        let run = add(&machine, "bad", OFF_LIST, &["--runtime", name]);
        assert_eq!(error_code(&run), "usage", "{name:?}: {}", run.stdout);
    }
    let url = machine.toon(&[
        "add",
        "served",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9",
        "--runtime",
        "runsc",
        "--yes",
        "--json",
    ]);
    assert_eq!(error_code(&url), "usage", "{}", url.stdout);

    let relay = machine.toon(&["change", "relay", "--runtime", "runsc", "--yes", "--json"]);
    assert_eq!(error_code(&relay), "usage", "{}", relay.stdout);
    assert_eq!(runtime(&machine, "relay"), Value::Null);

    assert_eq!(add(&machine, "ok", OFF_LIST, &[]).exit_code, 0);
    let unusable = machine.toon(&["change", "ok", "--runtime", "-x", "--yes", "--json"]);
    assert_eq!(error_code(&unusable), "usage");

    machine.write_agent_node_file("untrusted-runtime", "--privileged\n");
    let node = add(&machine, "off", OFF_LIST, &[]);
    assert_eq!(error_code(&node), "usage", "{}", node.stdout);
    // An image on the list, or one given a runtime, does not read the file.
    assert_eq!(add(&machine, "on", ON_LIST, &[]).exit_code, 0);
    assert_eq!(
        add(&machine, "named", OFF_LIST, &["--runtime", "runsc"]).exit_code,
        0
    );

    let status = machine.toon(&["status", "--json"]).stdout;
    assert!(
        !status.contains("\"name\":\"bad\"") && !status.contains("\"off\""),
        "{status}"
    );
}

#[test]
fn a_home_written_before_runtimes_loads_with_none() {
    let (machine, _chain, _up) = running();
    add(&machine, "notes", OFF_LIST, &[]);
    let path = machine.agent_node_home().join("state.json");
    let state: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(state["toon_apps"][0]["apps"][1].get("runtime").is_none());
    assert_eq!(runtime(&machine, "notes"), Value::Null);
}
