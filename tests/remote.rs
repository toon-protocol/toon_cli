mod support;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

/// An agent node that is up, and a home of nobody's for the agent to run in.
struct Node {
    machine: Machine,
    nowhere: tempfile::TempDir,
    _chain: FakeChain,
    _up: Foreground,
}

fn node() -> Node {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let created = machine.toon_with(
        &[
            "create",
            "second",
            "--image",
            "second:1",
            "--no-peer",
            "--clearnet",
            "second.example.com",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(created.exit_code, 0, "{}{}", created.stdout, created.stderr);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Node {
        machine,
        nowhere: tempfile::tempdir().expect("a directory"),
        _chain: chain,
        _up: up,
    }
}

impl Node {
    /// Create an agent key and return its key file and public half.
    fn create(&self, extra: &[&str]) -> (PathBuf, String) {
        create_key(&self.machine, extra)
    }

    /// Run `toon` as the agent does: `HOME` holds no agent node, and the key file is named
    /// in the environment.
    fn agent(&self, key: &Path, args: &[&str]) -> Run {
        self.agent_in(self.nowhere.path(), key, args)
    }

    fn agent_in(&self, home: &Path, key: &Path, args: &[&str]) -> Run {
        as_agent(&self.machine, home, key, args)
    }
}

/// Create an agent key on `machine` and return its key file and public half.
fn create_key(machine: &Machine, extra: &[&str]) -> (PathBuf, String) {
    let mut args = vec!["agent-key", "create", "--json"];
    args.extend_from_slice(extra);
    let run = machine.toon_with(&args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    });
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let json = run.json();
    (
        PathBuf::from(json["key_file"].as_str().expect("key file")),
        json["agent_key"]["public_key"].as_str().unwrap().to_owned(),
    )
}

/// Run `toon` with the key file `key` in the environment and `home` as `HOME`.
fn as_agent(machine: &Machine, home: &Path, key: &Path, args: &[&str]) -> Run {
    // The supervisor binds its order address a moment after the key is made.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run = machine.toon_with(args, |command| {
            command.env("HOME", home).env("TOON_AGENT_KEY", key);
        });
        if run.exit_code != 1 || !run.stdout.contains("did not answer") || Instant::now() > deadline
        {
            return run;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn status_and_logs_work_with_a_key_file_and_a_home_that_holds_no_agent_node() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status,logs"]);

    let status = node.agent(&key, &["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{}{}", status.stdout, status.stderr);
    let apps = status.json()["toon_apps"].clone();
    assert_eq!(apps[0]["name"], "relay", "{apps}");
    assert_eq!(apps.as_array().unwrap().len(), 1);
    assert!(!status
        .stdout
        .contains(node.machine.home().to_str().unwrap()));
    assert!(!node.nowhere.path().join(".toon").exists());

    let text = node.agent(&key, &["status"]);
    assert_eq!(text.exit_code, 0, "{}{}", text.stdout, text.stderr);
    assert!(
        text.stdout.contains("relay: connector running"),
        "{}",
        text.stdout
    );

    let logs = node.agent(&key, &["logs", "relay", "--connector", "--json", "-n", "5"]);
    assert_eq!(logs.exit_code, 0, "{}{}", logs.stdout, logs.stderr);
    assert!(logs.json()["lines"].is_array());

    let unknown = node.agent(&key, &["logs", "nowhere", "--connector", "--json"]);
    assert_eq!(unknown.exit_code, 1);
    assert_eq!(unknown.json()["error"]["code"], "unknown_name");

    // An app's own log is not an order: the agent is told to ask for the connector's.
    let own = node.agent(&key, &["logs", "relay", "--json"]);
    assert_eq!(own.json()["error"]["code"], "usage", "{}", own.stdout);
    assert!(own.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--connector"));
}

#[test]
fn the_flag_selects_the_key_too() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status"]);
    let run = node.machine.toon_with(
        &["--agent-key", key.to_str().unwrap(), "status", "--json"],
        |command| {
            command.env("HOME", node.nowhere.path());
        },
    );
    // Retry once the supervisor listens.
    let run = if run.exit_code == 0 {
        run
    } else {
        thread::sleep(Duration::from_secs(1));
        node.machine.toon_with(
            &["--agent-key", key.to_str().unwrap(), "status", "--json"],
            |command| {
                command.env("HOME", node.nowhere.path());
            },
        )
    };
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_home_that_exists_is_not_read() {
    let node = node();
    // A key granted on the second TOON app only: the home's own status would list both.
    let (key, _) = node.create(&["--scope", "status", "--toon-app", "second"]);
    let run = node.agent_in(node.machine.home(), &key, &["status", "--json"]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let apps = run.json()["toon_apps"].clone();
    assert_eq!(apps.as_array().unwrap().len(), 1, "{apps}");
    assert_eq!(apps[0]["name"], "second");

    let wallet = node.agent_in(node.machine.home(), &key, &["wallet", "show", "--json"]);
    assert_eq!(wallet.exit_code, 1);
    assert_eq!(wallet.json()["error"]["code"], "needs_home");
}

#[test]
fn a_command_that_needs_the_home_says_so_and_sends_nothing() {
    let node = node();
    let commands: [&[&str]; 9] = [
        &["wallet", "show"],
        &["limit", "set", "--max-per-day", "1"],
        &["init"],
        &["create", "third", "--image", "third:1", "--deposit", "1"],
        &["up"],
        &["down"],
        &["wallet", "backup", "--out", "backup.json"],
        &["wallet", "restore", "backup.json"],
        &["agent-key", "list"],
    ];
    for command in commands {
        let mut args = command.to_vec();
        args.push("--json");
        // Even with no key file to read: `needs_home` comes before anything else is tried.
        let run = node.agent(Path::new("/nonexistent/agent-key.json"), &args);
        assert_eq!(
            run.exit_code, 1,
            "{command:?}: {}{}",
            run.stdout, run.stderr
        );
        let error = run.json()["error"].clone();
        assert_eq!(error["code"], "needs_home", "{command:?}: {error}");
        let named = match command[0] {
            "wallet" | "limit" | "agent-key" => command[..2].join(" "),
            other => other.to_owned(),
        };
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains(&format!("`toon {named}`")),
            "{command:?}: {error}"
        );
        assert!(error["message"].as_str().unwrap().contains("operator"));
    }
    assert!(!node.nowhere.path().join(".toon").exists());
}

#[test]
fn a_send_without_yes_is_refused_as_it_is_with_the_home() {
    let node = node();
    let (key, _) = node.create(&[]);
    let relay = node.machine.relay_prefix();
    let run = node.agent(&key, &["send", &relay, "--amount", "1", "--json"]);
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "not_confirmed");
}

#[test]
fn a_send_and_what_is_left_of_the_limit_are_the_connectors() {
    let node = node();
    let (key, public) = node.create(&["--max-per-command", "5", "--max-per-day", "100"]);
    let relay = node.machine.relay_prefix();

    let sent = node.agent(&key, &["send", &relay, "--amount", "0", "--yes", "--json"]);
    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert_eq!(sent.json()["outcome"], "fulfilled");

    let before = node.agent(&key, &["limit", "show", "--json"]);
    assert_eq!(before.exit_code, 0, "{}{}", before.stdout, before.stderr);
    let limits = before.json()["limits"]["relay"].clone();
    assert_eq!(limits["per_command"], "5", "{limits}");
    assert_eq!(limits["per_day"], "100");
    assert_eq!(limits["remaining_today"], "100");

    let over = node.agent(&key, &["send", &relay, "--amount", "6", "--yes", "--json"]);
    assert_eq!(over.exit_code, 1, "{}{}", over.stdout, over.stderr);
    assert_eq!(
        over.json()["error"]["code"],
        "spending_limit",
        "{}",
        over.stdout
    );

    let spent = node.agent(&key, &["send", &relay, "--amount", "3", "--yes", "--json"]);
    assert!(spent.exit_code <= 1, "{}{}", spent.stdout, spent.stderr);
    let after = node.agent(&key, &["limit", "show"]);
    assert_eq!(after.exit_code, 0, "{}{}", after.stdout, after.stderr);
    assert!(
        after
            .stdout
            .contains("per command 5, per day 100, remaining today 97"),
        "{}",
        after.stdout
    );
    assert!(!public.is_empty());
}

#[test]
fn a_peering_outside_the_keys_scope_or_limit_is_refused_by_the_connector() {
    let node = node();
    let (no_peer, _) = node.create(&["--scope", "status,send"]);
    let run = node.agent(
        &no_peer,
        &[
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "5",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("scope"), "{message}");

    let unconfirmed = node.agent(
        &no_peer,
        &[
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "5",
            "--json",
        ],
    );
    assert_eq!(unconfirmed.json()["error"]["code"], "not_confirmed");
}

#[test]
fn a_revoked_key_is_refused_by_the_supervisor_and_the_connector() {
    let node = node();
    let (key, public) = node.create(&[]);
    let relay = node.machine.relay_prefix();
    for args in [
        vec!["status", "--json"],
        vec!["limit", "show", "--json"],
        vec!["send", &relay, "--amount", "0", "--yes", "--json"],
    ] {
        let run = node.agent(&key, &args);
        assert_eq!(run.exit_code, 0, "{args:?}: {}{}", run.stdout, run.stderr);
    }

    let revoked = node.machine.toon(&["agent-key", "revoke", &public]);
    assert_eq!(revoked.exit_code, 0, "{}{}", revoked.stdout, revoked.stderr);

    for args in [
        vec!["status", "--json"],
        vec!["logs", "relay", "--connector", "--json"],
        vec!["limit", "show", "--json"],
        vec!["send", &relay, "--amount", "0", "--yes", "--json"],
        vec![
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "1",
            "--yes",
            "--json",
        ],
    ] {
        let run = node.agent(&key, &args);
        assert_eq!(run.exit_code, 1, "{args:?}: {}{}", run.stdout, run.stderr);
        // Refused by whoever checks the key, not failed for some other reason.
        let message = run.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            message.contains("supervisor refused") || message.contains("401"),
            "{args:?}: {message}"
        );
    }
}

/// An agent node on `chain` that may peer over plain `http://`, its wallet funded, and up.
fn up_on(chain: &AnvilChain, funded: u128) -> (Machine, Foreground, String) {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, funded);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let address = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .to_owned();
    (machine, up, address)
}

#[test]
fn a_peering_is_written_with_the_key() {
    const DEPOSIT: u128 = 1_000_000;
    let chain = AnvilChain::start();
    let (near, _near_up, _) = up_on(&chain, DEPOSIT * 10);
    let (_far, _far_up, far) = up_on(&chain, DEPOSIT * 10);
    let (key, _) = create_key(&near, &["--scope", "peer"]);
    let nowhere = tempfile::tempdir().expect("a directory");

    let peered = as_agent(
        &near,
        nowhere.path(),
        &key,
        &[
            "peer",
            "add",
            &format!("http://{far}/ilp"),
            "--deposit",
            &DEPOSIT.to_string(),
            "--id",
            "far",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
    assert_eq!(peered.json()["peering"]["id"], "far", "{}", peered.stdout);
    assert!(!nowhere.path().join(".toon").exists());
}

/// The names of the apps behind the TOON app `toon_app`, as `toon status` with `key` shows.
fn apps_behind(node: &Node, key: &Path, toon_app: &str) -> Vec<String> {
    let status = node.agent(key, &["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{}{}", status.stdout, status.stderr);
    status.json()["toon_apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["name"] == toon_app)
        .map(|app| {
            app["apps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|app| app["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn error_code(run: &Run) -> String {
    run.json()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn an_agent_key_adds_an_image_on_the_list_and_removes_it() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status,apps"]);

    for (name, image) in [
        ("plain", "ghcr.io/toon-protocol/echo"),
        ("tagged", "ghcr.io/toon-protocol/echo:2.1"),
        (
            "pinned",
            "ghcr.io/toon-protocol/echo@sha256:0123456789012345678901234567890123456789012345678901234567890123",
        ),
    ] {
        let add = node.agent(
            &key,
            &["add", name, "--to", "relay", "--image", image, "--yes", "--json"],
        );
        assert_eq!(add.exit_code, 0, "{image}: {}{}", add.stdout, add.stderr);
        assert_eq!(add.json()["added"], name);
        assert_eq!(add.json()["toon_app"], "relay");
    }
    let behind = apps_behind(&node, &key, "relay");
    for name in ["plain", "tagged", "pinned"] {
        assert!(behind.contains(&name.to_owned()), "{behind:?}");
    }

    let remove = node.agent(&key, &["remove", "tagged", "--yes", "--json"]);
    assert_eq!(remove.exit_code, 0, "{}{}", remove.stdout, remove.stderr);
    assert_eq!(remove.json()["removed"], "tagged");
    assert!(!apps_behind(&node, &key, "relay").contains(&"tagged".to_owned()));

    // The same confirmation the home asks for.
    let unconfirmed = node.agent(&key, &["remove", "plain", "--json"]);
    assert_eq!(error_code(&unconfirmed), "confirmation_required");
    assert!(apps_behind(&node, &key, "relay").contains(&"plain".to_owned()));
}

#[test]
fn an_agent_key_does_not_add_what_the_operator_has_not_trusted() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status,apps"]);

    let refused: [&[&str]; 8] = [
        &["--image", "docker.io/someone/else:1"],
        &["--image", "ghcr.io/toon-protocolx/echo"],
        &["--url", "http://127.0.0.1:9999"],
        &["--image", "ghcr.io/toon-protocol/echo", "--allow-egress"],
        &["--image", "ghcr.io/toon-protocol/echo", "--memory", "1g"],
        &[
            "--image",
            "ghcr.io/toon-protocol/echo",
            "--scratch",
            "/tmp/x",
        ],
        &["--image", "ghcr.io/toon-protocol/echo", "--env", "A=b"],
        &["--image", "ghcr.io/toon-protocol/echo", "--cpus", "2"],
    ];
    for flags in refused {
        let mut args = vec!["add", "nope", "--to", "relay", "--yes", "--json"];
        args.extend_from_slice(flags);
        let run = node.agent(&key, &args);
        assert_eq!(run.exit_code, 1, "{flags:?}: {}{}", run.stdout, run.stderr);
        assert_eq!(
            error_code(&run),
            "needs_operator",
            "{flags:?}: {}",
            run.stdout
        );
        assert!(
            run.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("operator must add it"),
            "{flags:?}"
        );
    }
    assert_eq!(apps_behind(&node, &key, "relay"), vec!["relay".to_owned()]);

    // A secret is refused with `needs_home`, and its file is not read.
    let secret = node.agent(
        &key,
        &[
            "add",
            "nope",
            "--to",
            "relay",
            "--image",
            "ghcr.io/toon-protocol/echo",
            "--yes",
            "--secret",
            "TOKEN=/nonexistent/file",
            "--json",
        ],
    );
    assert_eq!(error_code(&secret), "needs_home", "{}", secret.stdout);
    assert_eq!(apps_behind(&node, &key, "relay"), vec!["relay".to_owned()]);
}

#[test]
fn the_operator_edits_the_list_and_the_home_is_never_checked_against_it() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status,apps"]);
    let list = node.machine.agent_node_home().join("trusted-images");
    let seeded = std::fs::read_to_string(&list).expect("`toon init` writes the list");
    assert!(seeded.contains("ghcr.io/toon-protocol/*"), "{seeded}");

    let args = [
        "add",
        "team",
        "--to",
        "relay",
        "--image",
        "example.com/team/app:1",
        "--yes",
        "--json",
    ];
    assert_eq!(error_code(&node.agent(&key, &args)), "needs_operator");

    std::fs::write(&list, "# mine\nexample.com/team/*\n").unwrap();
    let added = node.agent(&key, &args);
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);

    // The seed is replaced, not kept: the operator's file is the list.
    let gone = node.agent(
        &key,
        &[
            "add",
            "echo",
            "--to",
            "relay",
            "--image",
            "ghcr.io/toon-protocol/echo",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(error_code(&gone), "needs_operator");

    // With the home, an image not on the list is added as before.
    let home = node.machine.toon(&[
        "add",
        "other",
        "--to",
        "relay",
        "--image",
        "docker.io/someone/else:1",
        "--yes",
        "--json",
    ]);
    assert_eq!(home.exit_code, 0, "{}{}", home.stdout, home.stderr);
}

#[test]
fn an_agent_key_removes_any_app_but_the_relay() {
    let node = node();
    let (key, _) = node.create(&["--scope", "status,apps"]);
    let relay = node.agent(&key, &["remove", "relay", "--yes", "--json"]);
    assert_eq!(relay.exit_code, 1, "{}{}", relay.stdout, relay.stderr);
    assert_eq!(error_code(&relay), "needs_operator");
    assert!(apps_behind(&node, &key, "relay").contains(&"relay".to_owned()));

    let nothing = node.agent(&key, &["remove", "ghost", "--yes", "--json"]);
    assert_eq!(error_code(&nothing), "unknown_name");
}

#[test]
fn an_add_or_remove_needs_the_scope_and_a_toon_app_the_key_is_granted_on() {
    let node = node();
    let (without, _) = node.create(&["--scope", "status"]);
    let add = node.agent(
        &without,
        &[
            "add",
            "x",
            "--to",
            "relay",
            "--image",
            "ghcr.io/toon-protocol/echo",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(add.exit_code, 1, "{}{}", add.stdout, add.stderr);
    assert!(add.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("'apps'"));
    let remove = node.agent(&without, &["remove", "relay", "--yes", "--json"]);
    assert_eq!(remove.exit_code, 1);
    assert!(remove.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("'apps'"));

    // Granted on the relay's TOON app only: `second` and the app behind it are not its.
    let (key, _) = node.create(&["--scope", "status,apps"]);
    let add = node.agent(
        &key,
        &[
            "add",
            "x",
            "--to",
            "second",
            "--image",
            "ghcr.io/toon-protocol/echo",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(add.exit_code, 1, "{}{}", add.stdout, add.stderr);
    assert_eq!(error_code(&add), "unknown_name");
    let remove = node.agent(&key, &["remove", "second", "--yes", "--json"]);
    assert_eq!(error_code(&remove), "unknown_name");

    let status = node.machine.toon(&["status", "--json"]).json();
    let names: Vec<String> = status["agent_node"]["toon_apps"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|app| app["apps"].as_array().unwrap().iter())
        .map(|app| app["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"second".to_owned()), "{names:?}");
    assert!(!names.contains(&"x".to_owned()), "{names:?}");
}
