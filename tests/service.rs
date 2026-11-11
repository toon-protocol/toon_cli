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
    support::process_exists(pid)
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
             StartLimitIntervalSec=120\n\
             StartLimitBurst=5\n\
             \n\
             [Service]\n\
             Type=notify\n\
             TimeoutStartSec=600\n\
             Environment=HOME={}\n\
             ExecStart=\"{}\" up --foreground\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            machine.home().display(),
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

const OTHER_HOME_UNIT: &str = "[Unit]\nDescription=TOON agent node\n\n[Service]\nEnvironment=HOME=/home/someone-else\nExecStart=\"/bin/toon\" up --foreground\n";
const LEGACY_UNIT: &str =
    "[Unit]\nDescription=TOON agent node\n\n[Service]\nExecStart=\"/bin/toon\" up --foreground\n";

fn install_unit(machine: &Machine, text: &str) {
    let path = unit_path(machine);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn assert_refused(run: &support::Run, calls: &Path) {
    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(run.json()["error"]["code"], "unit_owned_by_another_home");
    assert!(!calls.exists(), "systemctl was called");
}

#[test]
fn up_refuses_a_unit_that_belongs_to_another_home() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);
    install_unit(&machine, OTHER_HOME_UNIT);
    let with_path = |command: &mut Command| {
        command.env("PATH", &bin);
    };

    let up = machine.toon_with(&["up", "--json"], with_path);

    assert_refused(&up, &calls);
    let message = up.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(message.contains("/home/someone-else"), "{message}");
    assert!(message.contains("--foreground"), "{message}");
    assert_eq!(
        fs::read_to_string(unit_path(&machine)).unwrap(),
        OTHER_HOME_UNIT
    );
    let text = machine.toon_with(&["up"], with_path);
    assert_eq!(text.exit_code, 1);
    assert!(
        text.stderr.contains("/home/someone-else"),
        "{}",
        text.stderr
    );
    assert!(text.stderr.contains("--foreground"), "{}", text.stderr);
}

#[test]
fn up_in_the_foreground_never_checks_the_unit() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    install_unit(&machine, OTHER_HOME_UNIT);

    let up = machine.start(&["up", "--foreground", "--json"]);

    assert!(up.report()["connector"]["pid"].as_u64().is_some());
    assert_eq!(
        fs::read_to_string(unit_path(&machine)).unwrap(),
        OTHER_HOME_UNIT
    );
}

#[test]
fn down_refuses_a_unit_that_belongs_to_another_home() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);
    install_unit(&machine, OTHER_HOME_UNIT);

    let down = machine.toon_with(&["down", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_refused(&down, &calls);
    assert_eq!(
        fs::read_to_string(unit_path(&machine)).unwrap(),
        OTHER_HOME_UNIT
    );
}

#[test]
fn a_legacy_unit_is_refused_from_a_home_that_is_not_the_login_home() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);
    install_unit(&machine, LEGACY_UNIT);

    let up = machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_refused(&up, &calls);
    assert_eq!(
        fs::read_to_string(unit_path(&machine)).unwrap(),
        LEGACY_UNIT
    );
}

#[test]
fn up_replaces_a_unit_of_the_same_home() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, _) = fake_systemd(&machine);
    install_unit(
        &machine,
        &format!("[Service]\nEnvironment=HOME={}\n", machine.home().display()),
    );

    let up = machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_eq!(up.exit_code, 0, "{}", up.stdout);
    assert!(fs::read_to_string(unit_path(&machine))
        .unwrap()
        .contains("up --foreground"));
}

#[test]
fn down_stops_a_unit_of_the_same_home() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, calls) = fake_systemd(&machine);
    install_unit(
        &machine,
        &format!("[Service]\nEnvironment=HOME={}\n", machine.home().display()),
    );

    let down = machine.toon_with(&["down", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    assert_eq!(
        fs::read_to_string(calls).unwrap(),
        "systemctl --user disable --now toon-agent-node.service\n"
    );
}

#[test]
fn up_without_systemctl_leaves_the_unit_and_says_why() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    // An empty `PATH`, so that the real `systemctl` of the machine running the tests is
    // never found.
    let empty = machine.home().join("empty-bin");
    fs::create_dir_all(&empty).unwrap();

    let up = machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &empty);
    });

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
fn down_says_so_when_systemctl_would_not_stop_the_unit() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let (bin, _) = fake_systemd(&machine);
    machine.toon_with(&["up", "--json"], |command| {
        command.env("PATH", &bin);
    });
    let refusing = bin.join("systemctl");
    fs::write(&refusing, "#!/bin/sh\necho 'Access denied' >&2\nexit 1\n").unwrap();

    let down = machine.toon_with(&["down", "--json"], |command| {
        command.env("PATH", &bin);
    });

    assert_eq!(down.json()["error"]["code"], "systemd_failed");
    assert_eq!(down.exit_code, 1);
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

/// Write `text` as the log of the app `name`, as the process runner does.
fn write_app_log(machine: &Machine, name: &str, text: &str) {
    let data = machine
        .agent_node_home()
        .join("apps")
        .join(name)
        .join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("app.log"), text).unwrap();
}

#[test]
fn logs_of_a_name_that_is_a_toon_app_and_an_app_shows_the_apps_log_and_connector_asks_for_the_other(
) {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let log = machine.agent_node_home().join("connectors/0/connector.log");
    fs::write(&log, "one\ntwo\nthree\n").unwrap();
    write_app_log(&machine, "relay", "app one\napp two\napp three\n");

    let run = machine.toon(&["logs", "relay", "-n", "2"]);
    assert_eq!(run.stdout, "app two\napp three\n");
    assert_eq!(run.exit_code, 0);
    let json = machine.toon(&["logs", "relay", "--json"]).json();
    assert_eq!(json["source"], "app");
    assert_eq!(
        json["lines"],
        serde_json::json!(["app one", "app two", "app three"])
    );
    let json = machine
        .toon(&["logs", "relay", "--connector", "--json"])
        .json();
    assert_eq!(json["source"], "connector");
    assert_eq!(json["lines"], serde_json::json!(["one", "two", "three"]));
    let run = machine.toon(&["logs", "relay", "--connector", "-n", "2"]);
    assert_eq!(run.stdout, "two\nthree\n");
}

#[test]
fn logs_of_an_app_behind_another_toon_app_name_shows_its_log_or_its_connectors() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let added = machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--yes", "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}", added.stdout);
    let log = machine.agent_node_home().join("connectors/0/connector.log");
    fs::write(&log, "connector line\n").unwrap();
    write_app_log(&machine, "notes", "notes line\n");

    let app = machine.toon(&["logs", "notes", "--json"]).json();
    assert_eq!(app["source"], "app");
    assert_eq!(app["toon_app"], "relay");
    assert_eq!(app["lines"], serde_json::json!(["notes line"]));
    let connector = machine
        .toon(&["logs", "notes", "--connector", "--json"])
        .json();
    assert_eq!(connector["source"], "connector");
    assert_eq!(connector["lines"], serde_json::json!(["connector line"]));
}

#[test]
fn logs_of_a_toon_app_with_no_app_of_that_name_shows_the_connectors_log() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let added = machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--yes", "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}", added.stdout);
    let removed = machine.toon(&["remove", "relay", "--yes", "--json"]);
    assert_eq!(removed.exit_code, 0, "{}", removed.stdout);
    let log = machine.agent_node_home().join("connectors/0/connector.log");
    fs::write(&log, "connector line\n").unwrap();

    let json = machine.toon(&["logs", "relay", "--json"]).json();

    assert_eq!(json["source"], "connector");
    assert_eq!(json["toon_app"], "relay");
    assert_eq!(json["lines"], serde_json::json!(["connector line"]));
}

#[test]
fn logs_of_an_app_that_failed_to_start_shows_what_it_wrote_while_the_agent_node_is_stopped() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    fs::write(machine.agent_node_home().join("apps/fail-relay"), "").unwrap();
    let up = machine.toon(&["up", "--foreground", "--json"]);
    assert_eq!(up.json()["error"]["code"], "app_failed", "{}", up.stdout);

    let run = machine.toon(&["logs", "relay", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["source"], "app");
    assert_eq!(
        run.json()["lines"],
        serde_json::json!(["fake relay: told not to start"])
    );
}

#[test]
fn logs_of_an_app_that_never_started_is_empty() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let json = machine.toon(&["logs", "relay", "--json"]).json();

    assert_eq!(json["source"], "app");
    assert_eq!(json["lines"], serde_json::json!([]));
}

#[test]
fn logs_of_an_app_served_at_a_url_says_it_has_none_and_names_the_flag() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let added = machine.toon(&[
        "add",
        "remote",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9/inbox",
        "--yes",
        "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}", added.stdout);

    let run = machine.toon(&["logs", "remote", "--json"]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert!(
        run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--connector"),
        "{}",
        run.stdout
    );
    let connector = machine.toon(&["logs", "remote", "--connector", "--json"]);
    assert_eq!(connector.exit_code, 0);
    assert_eq!(connector.json()["source"], "connector");
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
