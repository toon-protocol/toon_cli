mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

/// What the app `name` was started with, one `NAME=value` per line.
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

fn secrets_dir(machine: &Machine, name: &str) -> PathBuf {
    machine
        .agent_node_home()
        .join("apps")
        .join(name)
        .join("secrets")
}

#[test]
fn variables_and_secrets_reach_the_app_and_hold_across_down_and_up() {
    let (machine, _chain, up) = running();
    let key = file(&machine, "key", b"s3cret-value\n");
    let windows = file(&machine, "windows", b"crlf-value\r\n");
    let bytes = file(&machine, "bytes", b"raw\n\0bytes\n");
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--env",
        "A=1",
        "--env",
        "B=",
        "--env",
        "C=x=y",
        "--secret",
        &format!("KEY={key}"),
        "--secret",
        &format!("WIN={windows}"),
        "--secret-file",
        &format!("/run/secrets/raw={bytes}"),
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    drop(run);

    let check = |machine: &Machine| {
        let seen = environment(machine, "notes");
        for line in [
            "A=1",
            "B=",
            "C=x=y",
            "KEY=s3cret-value",
            "WIN=crlf-value",
            "TOON_DATA_DIR=",
        ] {
            assert!(seen.lines().any(|l| l.starts_with(line)), "{line}: {seen}");
        }
        assert!(seen.contains("TOON_BLS_PORT="), "{seen}");
    };
    check(&machine);

    let status = app(&machine, "notes");
    assert_eq!(status["variables"], json!({"A": "1", "B": "", "C": "x=y"}));
    assert_eq!(
        status["secrets"],
        json!({"variables": ["KEY", "WIN"], "files": ["/run/secrets/raw"]})
    );
    let text = machine.toon(&["status"]).stdout;
    assert!(text.contains("variable A"), "{text}");
    assert!(text.contains("secret KEY"), "{text}");
    assert!(text.contains("secret file /run/secrets/raw"), "{text}");
    assert!(!text.contains("s3cret-value"), "{text}");

    let dir = secrets_dir(&machine, "notes");
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!dir.starts_with(machine.agent_node_home().join("apps/notes/data")));
    let copies: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(copies.len(), 3);
    for copy in &copies {
        assert_eq!(
            fs::metadata(copy.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let state = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    assert!(!state.contains("s3cret-value"), "{state}");
    let config = fs::read_dir(machine.agent_node_home().join("connectors"))
        .unwrap()
        .flatten()
        .map(|entry| fs::read_to_string(entry.path().join("connector.toml")).unwrap_or_default())
        .collect::<String>();
    assert!(!config.contains("s3cret-value"), "{config}");

    // The operator's own files are gone, and the app is started the same.
    for name in ["key", "windows", "bytes"] {
        fs::remove_file(machine.home().join(name)).unwrap();
    }
    assert_eq!(machine.toon(&["down", "--json"]).exit_code, 0);
    drop(up);
    fs::remove_file(
        machine
            .agent_node_home()
            .join("apps/notes/data/all-environment"),
    )
    .unwrap();
    let _again = machine.start(&["up", "--foreground", "--json"]);
    _again.report();
    check(&machine);

    let removed = machine.toon(&["remove", "notes", "--yes", "--json"]);
    assert_eq!(removed.exit_code, 0, "{}", removed.stdout);
    assert!(!dir.exists());
}

#[test]
fn status_shows_nothing_for_an_app_without_and_null_for_an_app_at_a_url() {
    let (machine, _chain, _up) = running();
    machine.toon(&["add", "plain", "--to", "relay", "--image", "n:1", "--yes"]);
    machine.toon(&[
        "add",
        "served",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9",
        "--yes",
    ]);
    let plain = app(&machine, "plain");
    assert_eq!(plain["variables"], json!({}));
    assert_eq!(plain["secrets"], json!({"variables": [], "files": []}));
    let served = app(&machine, "served");
    assert_eq!(served["variables"], Value::Null);
    assert_eq!(served["secrets"], Value::Null);
    let text = machine.toon(&["status"]).stdout;
    assert!(!text.contains("is started with"), "{text}");
}

#[test]
fn a_home_written_before_variables_loads_with_none() {
    let (machine, _chain, _up) = running();
    let key = file(&machine, "key", b"old-secret\n");
    let run = machine.toon(&[
        "add",
        "plain",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--env",
        "A=1",
        "--secret",
        &format!("KEY={key}"),
        "--yes",
    ]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    // The record as a home written before variables and secrets holds it: without them.
    let path = machine.agent_node_home().join("state.json");
    let mut state: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for toon_app in state["toon_apps"].as_array_mut().unwrap() {
        for app in toon_app["apps"].as_array_mut().unwrap() {
            let record = app.as_object_mut().unwrap();
            record.remove("env");
            record.remove("secrets");
        }
    }
    fs::write(&path, state.to_string()).unwrap();
    let plain = app(&machine, "plain");
    assert_eq!(plain["variables"], json!({}));
    assert_eq!(plain["secrets"], json!({"variables": [], "files": []}));
}

#[test]
fn create_gives_its_app_variables_and_secrets_too() {
    let (machine, _chain, _up) = running();
    let key = file(&machine, "key", b"created-secret\n");
    let run = machine.toon_with(
        &[
            "create",
            "second",
            "--image",
            "n:1",
            "--no-peer",
            "--clearnet",
            "second.example.com",
            "--env",
            "A=1",
            "--secret",
            &format!("KEY={key}"),
            "--yes",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let seen = environment(&machine, "second");
    assert!(seen.lines().any(|l| l == "A=1"), "{seen}");
    assert!(seen.lines().any(|l| l == "KEY=created-secret"), "{seen}");
    assert!(!run.stdout.contains("created-secret"));
}

#[test]
fn what_cannot_be_used_is_refused_and_changes_nothing() {
    let (machine, _chain, _up) = running();
    let good = file(&machine, "good", b"v\n");
    let empty = file(&machine, "empty", b"");
    let big = file(&machine, "big", &vec![b'a'; 64 * 1024 + 1]);
    let binary = file(&machine, "binary", &[0xff, 0xfe]);
    let nul = file(&machine, "nul", b"a\0b");
    let lines = file(&machine, "lines", b"a\nb\n");
    let missing = machine.home().join("missing").display().to_string();
    // A directory exists and cannot be read as a file, whoever runs the test.
    let unreadable = machine.home().join("unreadable");
    fs::create_dir_all(&unreadable).unwrap();
    let unreadable = unreadable.display().to_string();
    let path = machine.agent_node_home().join("state.json");
    let before = fs::read_to_string(&path).unwrap();
    let cases: Vec<Vec<String>> = vec![
        vec!["--env".into(), "1A=x".into()],
        vec!["--env".into(), "A-B=x".into()],
        vec!["--env".into(), "=x".into()],
        vec!["--env".into(), "novalue".into()],
        vec!["--env".into(), "TOON_X=1".into()],
        vec!["--secret".into(), format!("TOON_X={good}")],
        vec![
            "--env".into(),
            "A=1".into(),
            "--secret".into(),
            format!("A={good}"),
        ],
        vec!["--env".into(), "A=1".into(), "--env".into(), "A=2".into()],
        vec!["--secret".into(), format!("A={missing}")],
        vec!["--secret".into(), format!("A={unreadable}")],
        vec!["--secret-file".into(), format!("/k={missing}")],
        vec!["--secret-file".into(), format!("/k={unreadable}")],
        vec!["--secret".into(), format!("A={empty}")],
        vec!["--secret".into(), format!("A={big}")],
        vec!["--secret".into(), format!("A={binary}")],
        vec!["--secret".into(), format!("A={nul}")],
        vec!["--secret".into(), format!("A={lines}")],
        vec!["--secret-file".into(), format!("relative={good}")],
        vec!["--secret-file".into(), format!("/data={good}")],
        vec!["--secret-file".into(), format!("/data/k={good}")],
        vec!["--secret-file".into(), format!("/tmp/k={good}")],
        vec!["--secret-file".into(), format!("/k={empty}")],
        vec!["--secret-file".into(), format!("/k={big}")],
        vec![
            "--scratch".into(),
            "/cache".into(),
            "--secret-file".into(),
            format!("/cache/k={good}"),
        ],
        vec![
            "--secret-file".into(),
            format!("/k={good}"),
            "--secret-file".into(),
            format!("/k={good}"),
        ],
        vec![
            "--secret-file".into(),
            format!("/run/k={good}"),
            "--secret-file".into(),
            format!("//run/./k/={good}"),
        ],
        vec![
            "--env".into(),
            "A=1".into(),
            "--url".into(),
            "http://127.0.0.1:9".into(),
        ],
    ];
    for extra in cases {
        for command in ["add", "create"] {
            let mut args: Vec<String> = if command == "add" {
                ["add", "notes", "--to", "relay"].map(String::from).to_vec()
            } else {
                ["create", "notes", "--no-peer", "--accept-anyone-terms"]
                    .map(String::from)
                    .to_vec()
            };
            if !extra.contains(&"--url".to_string()) {
                args.extend(["--image".into(), "n:1".into()]);
            }
            args.extend(extra.clone());
            args.extend(["--yes".into(), "--json".into()]);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let run = machine.toon(&refs);
            assert_eq!(run.exit_code, 2, "{args:?}: {}{}", run.stdout, run.stderr);
            assert!(run.stdout.contains("\"usage\""), "{args:?}: {}", run.stdout);
            assert_eq!(fs::read_to_string(&path).unwrap(), before, "{args:?}");
            assert!(!secrets_dir(&machine, "notes").exists());
        }
    }
    let status = machine.toon(&["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{}", status.stdout);
    let status = status.json();
    assert_eq!(
        status["agent_node"]["supervisor"]["running"], true,
        "{status}"
    );
}

#[test]
fn a_secret_is_in_no_output_when_the_app_fails_and_no_copy_is_left() {
    let (machine, _chain, _up) = running();
    fs::write(machine.agent_node_home().join("apps/fail-notes"), "").unwrap_or_else(|_| {
        fs::create_dir_all(machine.agent_node_home().join("apps")).unwrap();
        fs::write(machine.agent_node_home().join("apps/fail-notes"), "").unwrap();
    });
    let key = file(&machine, "key", b"leaky-secret-value\n");
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--secret",
        &format!("KEY={key}"),
        "--env",
        "LEVEL=debug-level",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("app_failed"), "{}", run.stdout);
    for output in [&run.stdout, &run.stderr] {
        assert!(!output.contains("leaky-secret-value"), "{output}");
        assert!(!output.contains("debug-level"), "{output}");
    }
    let status = machine.toon(&["status"]);
    assert!(!status.stdout.contains("leaky-secret-value"));
    assert!(!secrets_dir(&machine, "notes").exists());
}
