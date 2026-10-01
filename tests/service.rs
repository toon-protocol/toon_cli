mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use support::fake_chain::FakeChain;
use support::Machine;

fn unit_path(machine: &Machine) -> PathBuf {
    machine
        .home()
        .join(".config/systemd/user/toon-agent-node.service")
}

/// A directory holding `systemctl` and `loginctl` stand-ins that append their arguments
/// to `calls`, so that nothing here needs systemd.
fn fake_systemd(machine: &Machine) -> (PathBuf, PathBuf) {
    let bin = machine.home().join("fake-bin");
    let calls = machine.home().join("systemd-calls");
    fs::create_dir_all(&bin).unwrap();
    for program in ["systemctl", "loginctl"] {
        let script = bin.join(program);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"{program} $*\" >> '{}'\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (bin, calls)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn pid_alive(pid: u64) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[test]
fn up_writes_a_unit_that_runs_the_supervisor_in_the_foreground() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);

    let up = machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_eq!(up.exit_code, 0, "{}", up.stdout);
    assert_eq!(up.stderr, "");
    let toon = fs::canonicalize(env!("CARGO_BIN_EXE_toon")).unwrap();
    assert_eq!(
        fs::read_to_string(unit_path(&machine)).unwrap(),
        format!(
            "# Written by `toon up`. `toon up` overwrites it and `toon down` stops it.\n\
             [Unit]\n\
             Description=TOON agent node\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=\"{}\" up --foreground\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            toon.display()
        )
    );
    assert_eq!(
        fs::read_to_string(calls).unwrap(),
        "systemctl --user daemon-reload\n\
         systemctl --user enable --now toon-agent-node.service\n\
         loginctl enable-linger\n"
    );
    assert_eq!(up.json()["unit"]["name"], "toon-agent-node.service");
}

#[test]
fn up_without_systemctl_leaves_the_unit_and_says_why() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let up = machine.toon(&["up", "--json"]);

    assert_eq!(up.json()["error"]["code"], "systemd_failed");
    assert_eq!(up.exit_code, 1);
    assert!(unit_path(&machine).exists());
}

#[test]
fn up_on_a_machine_with_no_agent_node_installs_nothing() {
    let machine = Machine::new();
    let (bin, calls) = fake_systemd(&machine);

    let up = machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_eq!(up.json()["error"]["code"], "no_agent_node");
    assert_eq!(up.exit_code, 3);
    assert!(!unit_path(&machine).exists());
    assert!(!calls.exists());
}

#[test]
fn down_stops_the_unit() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);
    let with_path = |command: &mut Command| {
        command.env("PATH", &bin);
    };
    machine.toon_with(&["up", "--json"], with_path);
    fs::remove_file(&calls).unwrap();

    let down = machine.toon_with(&["down", "--json"], with_path);

    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    assert_eq!(
        fs::read_to_string(calls).unwrap(),
        "systemctl --user disable --now toon-agent-node.service\n"
    );
}

#[test]
fn a_connector_that_exits_is_restarted_and_status_shows_it() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let first = up.report()["connector"]["pid"].as_u64().expect("a pid");

    let killed = Command::new("kill")
        .arg(first.to_string())
        .status()
        .unwrap();
    assert!(killed.success());

    let mut status = serde_json::Value::Null;
    wait_until("the connector is running again", || {
        status = machine.toon(&["status", "--json"]).json();
        status["agent_node"]["toon_apps"][0]["connector"]["restarts"] == 1
            && status["agent_node"]["toon_apps"][0]["connector"]["running"] == true
    });
    let connector = &status["agent_node"]["toon_apps"][0]["connector"];
    assert_ne!(connector["pid"].as_u64(), Some(first));
    assert!(!pid_alive(first));
    assert!(connector["last_exit"].is_string());
    let text = machine.toon(&["status"]);
    assert_eq!(text.exit_code, 0, "{}", text.stdout);
    assert!(
        text.stdout.contains("restarted once"),
        "status says so: {}",
        text.stdout
    );
}

#[test]
fn logs_shows_the_log_of_the_relay_toon_app_and_app() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let log = machine.agent_node_home().join("connectors/0/connector.log");
    fs::write(&log, "one\ntwo\nthree\n").unwrap();

    let run = machine.toon(&["logs", "relay", "-n", "2"]);
    assert_eq!(run.stdout, "two\nthree\n");
    assert_eq!(run.exit_code, 0);
    let json = machine.toon(&["logs", "relay", "--json"]).json();
    assert_eq!(json["lines"], serde_json::json!(["one", "two", "three"]));
}

#[test]
fn logs_of_a_name_that_is_not_there_says_so() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let run = machine.toon(&["logs", "nope", "--json"]);

    assert_eq!(run.json()["error"]["code"], "unknown_name");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn logs_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["logs", "relay", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}
