mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::Machine;

fn connector_dir(machine: &Machine) -> PathBuf {
    machine.agent_node_home().join("connectors").join("0")
}

/// Every file under `dir`, with its contents, in a stable order.
fn tree(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(tree(&path));
        } else {
            found.push((path.clone(), fs::read(&path).unwrap()));
        }
    }
    found.sort();
    found
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn init_records_the_first_toon_app_and_writes_its_keys() {
    let chain = FakeChain::start();
    let machine = Machine::new();

    let init = machine.init_on(&chain);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(init.stderr, "");
    let report = init.json();
    assert_eq!(report["toon_apps"][0]["name"], "relay");
    assert_eq!(report["toon_apps"][0]["created"], true);
    let state: Value =
        serde_json::from_slice(&fs::read(machine.agent_node_home().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["toon_apps"][0]["name"], "relay");
    assert_eq!(state["toon_apps"][0]["apps"][0], "relay");
    for key in ["identity.key", "settlement.key"] {
        let file = connector_dir(&machine).join(key);
        assert_eq!(fs::read(&file).unwrap().len(), 32);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(connector_dir(&machine).join("connector.toml").is_file());
}

#[test]
fn init_twice_creates_nothing_twice() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let before = tree(&machine.agent_node_home());

    let again = machine.toon(&["init", "--json", "--accept-anyone-terms"]);

    assert_eq!(again.exit_code, 0, "{}", again.stdout);
    let report = again.json();
    assert_eq!(report["created"], false);
    assert_eq!(report["toon_apps"][0]["created"], false);
    assert_eq!(tree(&machine.agent_node_home()), before);
}

#[test]
fn init_that_the_connector_would_refuse_leaves_no_wallet() {
    let machine = Machine::new();

    let run = machine.toon_with(
        &[
            "init",
            "--json",
            "--accept-anyone-terms",
            "--listen",
            "not an address",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(run.json()["error"]["code"], "connector_failed");
    assert_eq!(run.exit_code, 1);
    assert!(!machine.agent_node_home().join("keystore.json").exists());
    assert!(!machine.agent_node_home().join("state.json").exists());
    assert!(!machine.agent_node_home().join("connectors").exists());
}

#[test]
fn init_picks_a_port_the_system_does_not_hand_out_by_itself() {
    let machine = Machine::new();
    // Without `--listen`, which the other tests give: `init` picks the port.
    let init = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    // `init` picks the connector's port and every `up` binds it, later. The system hands
    // its ephemeral ports to any socket bound to port 0 and to any outgoing connection,
    // so a port picked among them can be taken by the time the connector binds it.
    let state: Value =
        serde_json::from_slice(&fs::read(machine.agent_node_home().join("state.json")).unwrap())
            .unwrap();
    let listen: std::net::SocketAddr = state["toon_apps"][0]["listen"]
        .as_str()
        .and_then(|listen| listen.parse().ok())
        .unwrap_or_else(|| panic!("the first TOON app has no listen address: {state}"));
    let range = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    let bounds: Vec<u16> = range
        .split_whitespace()
        .map(|bound| bound.parse().unwrap())
        .collect();
    assert!(
        !(bounds[0]..=bounds[1]).contains(&listen.port()),
        "{listen} is in the system's ephemeral range, {}",
        range.trim()
    );
}

#[test]
fn the_connector_config_is_rendered_from_the_state_on_every_up() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let config = connector_dir(&machine).join("connector.toml");
    let rendered = fs::read_to_string(&config).unwrap();
    fs::write(&config, "this is not a config\n").unwrap();

    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    // What changes is where the relay was found: `init` could only point at where its
    // container will serve, and `up` knows where the relay is. The overlay's proxy is
    // another: each overlay that starts picks its own port.
    let without_proxy = |config: String| {
        config
            .lines()
            .filter(|line| !line.starts_with("socks_proxy"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let status = machine.toon(&["status", "--json"]).json();
    let relay = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        without_proxy(fs::read_to_string(&config).unwrap()),
        without_proxy(rendered.replace("127.0.0.1:3100", &relay))
    );
}

#[test]
fn status_reports_the_running_connector_and_down_stops_everything() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let mut up = machine.start(&["up", "--foreground", "--json"]);
    let started = up.report();

    let status = machine.toon(&["status", "--json"]);

    assert_eq!(status.exit_code, 0, "{}", status.stdout);
    let report = status.json();
    let node = &report["agent_node"];
    assert_eq!(node["supervisor"]["running"], true);
    let app = &node["toon_apps"][0];
    assert_eq!(app["name"], "relay");
    assert_eq!(app["connector"]["running"], true);
    assert_eq!(app["connector"]["address"], started["connector"]["address"]);
    assert_eq!(app["connector"]["pid"], started["connector"]["pid"]);

    let down = machine.toon(&["down", "--json"]);

    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    assert_eq!(down.json()["stopped"], true);
    assert_eq!(up.exit_code(), 0);
    let pid = started["connector"]["pid"].as_u64().unwrap();
    wait_until("the connector is still running", || {
        !std::path::Path::new("/proc").join(pid.to_string()).exists()
    });
    let after = machine.toon(&["status", "--json"]);
    assert_eq!(after.exit_code, 1);
    assert_eq!(after.json()["agent_node"]["supervisor"]["running"], false);
}

#[test]
fn status_exits_non_zero_when_the_connector_is_down() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let status = machine.toon(&["status", "--json"]);

    assert_eq!(status.exit_code, 1);
    assert_eq!(status.stderr, "");
    let report = status.json();
    let connector = &report["agent_node"]["toon_apps"][0]["connector"];
    assert_eq!(connector["running"], false);
    assert_eq!(connector["address"], Value::Null);
}

#[test]
fn down_when_nothing_runs_is_not_an_error() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let down = machine.toon(&["down", "--json"]);

    assert_eq!(down.exit_code, 0);
    assert_eq!(down.json()["stopped"], false);
}

#[test]
fn down_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let down = machine.toon(&["down", "--json"]);

    assert_eq!(down.json()["error"]["code"], "no_agent_node");
    assert_eq!(down.exit_code, 3);
}

#[test]
fn a_second_up_is_refused_while_one_runs() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let second = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(second.json()["error"]["code"], "already_running");
    assert_eq!(second.exit_code, 1);
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 0);
}

#[test]
fn up_after_a_supervisor_was_killed_replaces_its_stale_socket() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let mut first = machine.start(&["up", "--foreground", "--json"]);
    first.report();
    first.kill();

    let second = machine.start(&["up", "--foreground", "--json"]);

    second.report();
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 0);
}

#[test]
fn up_refuses_a_state_with_no_toon_app() {
    let machine = Machine::new();
    machine.write_agent_node_file("state.json", r#"{"version": 1, "toon_apps": []}"#);

    let run = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(run.json()["error"]["code"], "io");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn a_home_with_a_long_path_has_a_working_supervisor() {
    let chain = FakeChain::start();
    let machine = Machine::with_long_home();
    let socket = machine.agent_node_home().join("supervisor.sock");
    assert!(socket.as_os_str().len() > 107, "the socket path is long");
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    assert!(up.report().get("error").is_none(), "up started");

    let status = machine.toon(&["status", "--json"]);
    assert_eq!(status.exit_code, 0);
    let status = status.json();
    assert_eq!(status["agent_node"]["supervisor"]["running"], true);
    assert_eq!(
        status["agent_node"]["supervisor"]["socket"],
        socket.to_str().unwrap()
    );

    let second = machine.toon(&["up", "--foreground", "--json"]);
    assert_eq!(second.json()["error"]["code"], "already_running");
    assert_eq!(second.exit_code, 1);

    let down = machine.toon(&["down", "--json"]);
    assert_eq!(down.exit_code, 0);
    assert_eq!(down.json()["stopped"], true);
}
