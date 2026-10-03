mod support;

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, PASSPHRASE};

/// Two keys that are points on the curve: the secrets 1 and 2.
const ALICE: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const BOB: &str = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";

/// An agent node, running, with the fake relay behind its connector.
struct Running {
    machine: Machine,
    _up: Foreground,
    _chain: FakeChain,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
    }
}

impl Running {
    fn toon(&self, args: &[&str]) -> support::Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    }

    fn send(&self, args: &[&str]) -> support::Run {
        let mut all = vec!["message", "send", "--json"];
        all.extend_from_slice(args);
        self.toon(&all)
    }

    fn relay_url(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"));
        format!("ws://{address}")
    }

    /// The events of `kinds` the agent node's own relay holds.
    fn events(&self, kinds: &[u64]) -> Vec<Value> {
        let filter = serde_json::json!({ "kinds": kinds }).to_string();
        let query = self.machine.toon(&[
            "event",
            "query",
            &self.relay_url(),
            "--filter",
            &filter,
            "--json",
        ]);
        assert_eq!(query.exit_code, 0, "{}", query.stdout);
        query.json()["events"].as_array().unwrap().clone()
    }

    fn agent_identity(&self) -> String {
        self.toon(&["wallet", "show", "--json"]).json()["wallet"]["agent_identity"]
            .as_str()
            .expect("the agent identity")
            .to_owned()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn p_tags(event: &Value) -> Vec<String> {
    event["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tag| tag[0] == "p")
        .map(|tag| tag[1].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_message_leaves_two_wraps_on_the_own_relay_and_nothing_else() {
    let node = running();
    let identity = node.agent_identity();

    let sent = node.send(&[ALICE, "--content", "hello"]);

    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    let report = sent.json();
    assert_eq!(report["outcome"], "sent", "{report}");
    assert_eq!(report["paid"], 0);
    assert_eq!(report["recipients"][0], ALICE);
    let rumor = &report["rumor"];
    assert_eq!(rumor["kind"], 14);
    assert_eq!(rumor["pubkey"], identity);
    assert_eq!(rumor["content"], "hello");
    assert_eq!(rumor["tags"], serde_json::json!([["p", ALICE]]));
    assert!(rumor["id"].as_str().unwrap().len() == 64);
    assert!(rumor.get("sig").is_none());

    let wraps = node.events(&[1059]);
    assert_eq!(wraps.len(), 2, "{wraps:?}");
    let mut addressed: Vec<String> = wraps.iter().flat_map(p_tags).collect();
    addressed.sort();
    let mut expected = vec![ALICE.to_owned(), identity.clone()];
    expected.sort();
    assert_eq!(addressed, expected);
    let ids: Vec<&str> = report["wraps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|wrap| wrap["id"].as_str().unwrap())
        .collect();
    for wrap in &wraps {
        assert!(ids.contains(&wrap["id"].as_str().unwrap()));
    }

    let keys: HashSet<&str> = wraps
        .iter()
        .map(|wrap| wrap["pubkey"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 2, "the wraps share a key");
    assert!(!keys.contains(identity.as_str()));
    for wrap in &wraps {
        assert!(wrap["created_at"].as_u64().unwrap() <= now());
        assert!(!wrap["content"].as_str().unwrap().contains("hello"));
        assert!(!wrap.to_string().contains(&identity) || p_tags(wrap) == [identity.clone()]);
    }

    // The rumor and the seal are never written.
    assert_eq!(node.events(&[13, 14]), Vec::<Value>::new());
}

#[test]
fn two_recipients_make_three_wraps() {
    let node = running();

    let sent = node.send(&[ALICE, BOB, "--content", "hi both"]);

    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    let report = sent.json();
    assert_eq!(
        report["rumor"]["tags"],
        serde_json::json!([["p", ALICE], ["p", BOB]])
    );
    assert_eq!(node.events(&[1059]).len(), 3);
    assert_eq!(report["wraps"].as_array().unwrap().len(), 3);
}

#[test]
fn a_reply_and_a_subject_are_tags_of_the_rumor() {
    let node = running();
    let reply = "ab".repeat(32);

    let sent = node.send(&[
        ALICE,
        "--content",
        "re",
        "--reply-to",
        &reply,
        "--subject",
        "plans",
    ]);

    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert_eq!(
        sent.json()["rumor"]["tags"],
        serde_json::json!([
            ["p", ALICE],
            ["e", reply, "", "reply"],
            ["subject", "plans"]
        ])
    );
}

#[test]
fn a_message_without_a_valid_recipient_or_content_is_a_usage_error() {
    let node = running();
    for args in [
        vec!["--content", "hi"],
        vec!["nothex", "--content", "hi"],
        vec!["1234", "--content", "hi"],
        // 32 bytes that are no point on the curve.
        vec![
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "--content",
            "hi",
        ],
        vec![ALICE, "--content", ""],
        vec![ALICE],
    ] {
        let sent = node.send(&args);
        assert_eq!(
            sent.exit_code, 2,
            "{args:?}: {}{}",
            sent.stdout, sent.stderr
        );
    }
    assert_eq!(node.events(&[1059]), Vec::<Value>::new());
}

#[test]
fn there_is_no_kind_and_no_tags_option() {
    let node = running();
    for flag in ["--kind", "--tags"] {
        let sent = node.send(&[ALICE, "--content", "hi", flag, "1"]);
        assert_eq!(sent.exit_code, 2, "{flag}");
    }
}

#[test]
fn message_is_in_the_help() {
    let machine = Machine::new();
    let help = machine.toon(&["--help"]);
    assert!(help.stdout.contains("message"), "{}", help.stdout);
}

fn kept(machine: &Machine) -> std::path::PathBuf {
    machine.agent_node_home().join("agent.key")
}

fn assert_kept_for(machine: &Machine, identity: &str) {
    let path = kept(machine);
    let mode = fs::metadata(&path)
        .expect("the kept secret")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // The secret is the one the identity's public key comes from: sign with it.
    let secret = fs::read(&path).unwrap();
    assert_eq!(secret.len(), 32);
    let events = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    });
    assert_eq!(events.json()["wallet"]["agent_identity"], identity);
}

#[test]
fn init_keeps_the_agent_identitys_secret_and_a_backup_does_not_hold_it() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let identity = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    });
    let identity = identity.json()["wallet"]["agent_identity"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_kept_for(&machine, &identity);

    let file = machine.home().join("agent-node.backup");
    let backup = machine.toon_with(
        &[
            "wallet",
            "backup",
            "--json",
            "--out",
            file.to_str().unwrap(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(backup.exit_code, 0, "{}", backup.stdout);
    let secret = fs::read(kept(&machine)).unwrap();
    let backup = fs::read(&file).unwrap();
    assert!(!backup.windows(32).any(|window| window == secret.as_slice()));

    let after = Machine::new();
    let restore = after.toon_with(
        &["wallet", "restore", "--json", file.to_str().unwrap()],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(restore.exit_code, 0, "{}", restore.stdout);
    assert_eq!(fs::read(kept(&after)).unwrap(), secret);
    assert_kept_for(&after, &identity);
}

#[test]
fn init_from_a_mnemonic_keeps_the_same_secret() {
    let chain = FakeChain::start();
    let before = Machine::new();
    let created = before.init_on(&chain);
    let mnemonic = created.json()["mnemonic"].as_str().unwrap().to_owned();

    let after = Machine::new();
    let restored = after.toon_with(
        &[
            "init",
            "--json",
            "--accept-anyone-terms",
            "--from-mnemonic",
            "--evm-rpc-url",
            &chain.rpc_url(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            command.env("TOON_MNEMONIC", &mnemonic);
        },
    );
    assert_eq!(restored.exit_code, 0, "{}", restored.stdout);

    assert_eq!(
        fs::read(kept(&after)).unwrap(),
        fs::read(kept(&before)).unwrap()
    );
}

#[test]
fn event_publish_and_message_send_keep_the_secret_where_it_is_missing() {
    let node = running();
    let identity = node.agent_identity();
    let path = kept(&node.machine);

    fs::remove_file(&path).unwrap();
    let published = node.toon(&[
        "event",
        "publish",
        "--kind",
        "1",
        "--content",
        "a",
        "--json",
    ]);
    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    assert_kept_for(&node.machine, &identity);

    fs::remove_file(&path).unwrap();
    let sent = node.send(&[ALICE, "--content", "hi"]);
    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert_kept_for(&node.machine, &identity);
}

#[test]
fn where_the_senders_copy_cannot_be_written_nothing_is_sent() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    // The agent node is not running, so its own relay cannot take the sender's copy.
    let refused = machine.toon_with(
        &["message", "send", ALICE, "--content", "hello", "--json"],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_ne!(refused.exit_code, 0, "{}", refused.stdout);
    assert!(
        refused.json()["error"]["code"].is_string(),
        "{}",
        refused.stdout
    );

    let up = machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    let node = Running {
        machine,
        _up: up,
        _chain: chain,
    };
    assert_eq!(node.events(&[1059]), Vec::<Value>::new());
}
