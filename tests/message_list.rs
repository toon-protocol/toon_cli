mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, PASSPHRASE};

/// An agent node, running, with the fake relay behind its connector.
struct Node {
    machine: Machine,
    up: Option<Foreground>,
}

fn node_on(chain: &FakeChain) -> Node {
    let machine = Machine::new();
    let init = machine.init_on(chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    Node {
        machine,
        up: Some(up),
    }
}

impl Node {
    /// `toon` with the passphrase, as a command that signs is run.
    fn toon(&self, args: &[&str]) -> support::Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    }

    fn identity(&self) -> String {
        self.toon(&["wallet", "show", "--json"]).json()["wallet"]["agent_identity"]
            .as_str()
            .expect("the agent identity")
            .to_owned()
    }

    fn send(&self, args: &[&str]) {
        let mut all = vec!["message", "send", "--json"];
        all.extend_from_slice(args);
        let sent = self.toon(&all);
        assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    }

    /// The address the relay is written at.
    fn relay(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"))
            .to_owned()
    }

    /// The wraps this node's relay holds that are addressed to `key`.
    fn wraps_for(&self, key: &str) -> Vec<Value> {
        let mut wraps = self.machine.stored_events(&[1059]);
        wraps.retain(|wrap| {
            wrap["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag[0] == "p" && tag[1] == key))
        });
        wraps
    }

    /// Write `event` to this node's relay as a relay is written to: its write route.
    fn write(&self, event: &Value) {
        let body = json!({ "event": event }).to_string();
        let mut stream = TcpStream::connect(self.relay()).expect("the relay's write port");
        write!(
            stream,
            "POST /write HTTP/1.1\r\nHost: relay\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    fn list(&self, args: &[&str]) -> support::Run {
        let mut all = vec!["message", "list", "--json"];
        all.extend_from_slice(args);
        // No passphrase: listing opens no keystore.
        self.machine.toon(&all)
    }

    fn messages(&self, args: &[&str]) -> Vec<Value> {
        let run = self.list(args);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        run.json()["messages"].as_array().unwrap().clone()
    }

    /// Wait until the messages listed number `count`.
    fn wait_for(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let messages = self.messages(&[]);
            if messages.len() >= count {
                return messages;
            }
            assert!(
                Instant::now() < deadline,
                "{count} messages were not opened; there are {messages:?}\n{}",
                self.up.as_ref().map(Foreground::stderr).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(200));
        }
    }

    fn store(&self) -> std::path::PathBuf {
        self.machine.agent_node_home().join("private-messages.json")
    }
}

/// `from` sends `content` to `to` (and every key in `others`) and the wrap for `to` is
/// written to `at`'s relay, as a subscription of `at` would write it.
fn deliver(from: &Node, to: &str, others: &[&str], content: &str, at: &Node) {
    let mut args = vec![to];
    args.extend_from_slice(others);
    args.extend_from_slice(&["--content", content]);
    from.send(&args);
    for wrap in from.wraps_for(to) {
        at.write(&wrap);
    }
}

#[test]
fn a_sent_message_is_listed_without_a_passphrase() {
    let chain = FakeChain::start();
    let alice = node_on(&chain);
    let bob = node_on(&chain);
    let (a, b) = (alice.identity(), bob.identity());

    alice.send(&[&b, "--content", "hello"]);

    let messages = alice.wait_for(1);
    assert_eq!(messages.len(), 1, "{messages:?}");
    let message = &messages[0];
    assert_eq!(message["sent"], true);
    assert_eq!(message["from"], a.as_str());
    assert_eq!(message["content"], "hello");
    assert_eq!(message["kind"], 14);
    let mut both = vec![a.clone(), b.clone()];
    both.sort();
    assert_eq!(message["participants"], json!(both));
    assert_eq!(message["tags"], json!([["p", b]]));
    assert!(message["created_at"].as_u64().is_some());
    assert_eq!(message["id"].as_str().unwrap().len(), 64);
    assert_eq!(alice.list(&[]).json()["supervisor"], "running");

    let mode = fs::metadata(alice.store()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // The store is a cache: a backup does not carry it.
    let file = alice.machine.home().join("agent-node.backup");
    let backup = alice.toon(&[
        "wallet",
        "backup",
        "--json",
        "--out",
        file.to_str().unwrap(),
    ]);
    assert_eq!(backup.exit_code, 0, "{}", backup.stdout);
    let after = Machine::new();
    let restore = after.toon_with(
        &["wallet", "restore", "--json", file.to_str().unwrap()],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(restore.exit_code, 0, "{}", restore.stdout);
    assert!(after.agent_node_home().is_dir());
    assert!(!after
        .agent_node_home()
        .join("private-messages.json")
        .exists());
}

fn a_message_from_another_key_appears_live(alice: &Node, bob: &Node) {
    let (a, b) = (alice.identity(), bob.identity());
    assert!(alice.messages(&[]).is_empty());

    deliver(bob, &a, &[], "from bob", alice);

    let messages = alice.wait_for(1);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0]["sent"], false);
    assert_eq!(messages[0]["from"], b.as_str());
    assert_eq!(messages[0]["content"], "from bob");
}

#[test]
fn a_wrap_from_another_key_appears_without_a_restart() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    a_message_from_another_key_appears_live(&alice, &bob);
}

#[test]
fn it_appears_too_where_the_relay_sells_its_feed() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let priced = alice.toon(&[
        "relay",
        "price",
        "--subscribe",
        "1000",
        "--broadcast",
        "10",
        "--yes",
        "--json",
    ]);
    assert_eq!(priced.exit_code, 0, "{}", priced.stdout);
    a_message_from_another_key_appears_live(&alice, &bob);
}

#[test]
fn a_wrap_that_is_not_for_the_agent_or_does_not_open_is_skipped() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let (a, b) = (alice.identity(), bob.identity());
    let garbage = |id: &str, key: &str, content: &str| {
        json!({
            "id": id.repeat(64), "pubkey": b, "created_at": 1_700_000_000, "kind": 1059,
            "tags": [["p", key]], "content": content, "sig": "00".repeat(64),
        })
    };

    // A wrap for another key, and one for the agent whose content is not a seal.
    alice.write(&garbage("1", &b, "not for the agent"));
    alice.write(&garbage("2", &a, "garbage"));
    // A wrap the relay holds that the agent made for itself is opened.
    alice.send(&[&b, "--content", "still works"]);
    deliver(&bob, &a, &[], "later", &alice);

    let messages = alice.wait_for(2);
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(alice.list(&[]).json()["supervisor"], "running");
}

#[test]
fn deleting_the_store_and_restarting_brings_every_message_back_once() {
    let chain = FakeChain::start();
    let (mut alice, bob) = (node_on(&chain), node_on(&chain));
    let a = alice.identity();
    deliver(&bob, &a, &[], "one", &alice);
    alice.send(&[&bob.identity(), "--content", "two"]);
    let before = alice.wait_for(2);

    let down = alice.machine.toon(&["down", "--json"]);
    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    drop(alice.up.take());
    fs::remove_file(alice.store()).unwrap();
    let stopped = alice.list(&[]);
    assert_eq!(stopped.exit_code, 0, "{}", stopped.stdout);
    assert_eq!(stopped.json()["messages"], json!([]));
    assert_eq!(stopped.json()["supervisor"], "not_running");

    let up = alice.machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    alice.up = Some(up);
    let after = alice.wait_for(2);
    thread::sleep(Duration::from_secs(1));

    let mut ids: Vec<&str> = after.iter().filter_map(|m| m["id"].as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "{after:?}");
    assert_eq!(alice.messages(&[]).len(), 2);
    let mut kept: Vec<&str> = before.iter().filter_map(|m| m["id"].as_str()).collect();
    kept.sort();
    assert_eq!(kept, ids);
}

#[test]
fn a_stopped_supervisor_still_lists_what_is_stored() {
    let chain = FakeChain::start();
    let mut alice = node_on(&chain);
    alice.send(&[&alice.identity(), "--content", "to myself"]);
    alice.wait_for(1);

    let down = alice.machine.toon(&["down", "--json"]);
    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    drop(alice.up.take());

    let run = alice.list(&[]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["supervisor"], "not_running");
    assert_eq!(run.json()["messages"].as_array().unwrap().len(), 1);
    let text = alice.machine.toon(&["message", "list"]);
    assert_eq!(text.exit_code, 0);
    assert!(text.stdout.contains("to myself"), "{}", text.stdout);
    assert!(text.stdout.contains("not running"), "{}", text.stdout);
}

#[test]
fn conversations_are_the_keys_that_share_them() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let (a, b) = (alice.identity(), bob.identity());
    let carol = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";

    alice.send(&[&b, "--content", "one"]);
    deliver(&bob, &a, &[], "two", &alice);
    alice.send(&[&b, carol, "--content", "three"]);
    let all = alice.wait_for(3);

    let by_content = |content: &str| {
        all.iter()
            .find(|message| message["content"] == content)
            .unwrap_or_else(|| panic!("no {content}: {all:?}"))
    };
    assert_eq!(
        by_content("one")["conversation"],
        by_content("two")["conversation"]
    );
    assert_ne!(
        by_content("one")["conversation"],
        by_content("three")["conversation"]
    );
    let mut keys = [a.clone(), b.clone()];
    keys.sort();
    let expected = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(keys.join(",").as_bytes()))
    };
    assert_eq!(by_content("one")["conversation"], expected.as_str());

    let pair = alice.messages(&["--with", &b]);
    let contents: Vec<&str> = pair.iter().filter_map(|m| m["content"].as_str()).collect();
    assert_eq!(pair.len(), 2, "{contents:?}");
    assert!(contents.contains(&"one") && contents.contains(&"two"));
    let group = alice.messages(&["--with", &b, carol]);
    assert_eq!(group.len(), 1);
    assert_eq!(group[0]["content"], "three");
    assert!(alice.messages(&["--with", carol]).is_empty());
    let usage = alice.list(&["--with", "nothex"]);
    assert_eq!(usage.exit_code, 2, "{}", usage.stdout);
}

#[test]
fn since_and_limit_keep_the_newest_and_print_oldest_first() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let b = bob.identity();
    alice.send(&[&b, "--content", "first"]);
    thread::sleep(Duration::from_millis(1100));
    alice.send(&[&b, "--content", "second"]);
    thread::sleep(Duration::from_millis(1100));
    alice.send(&[&b, "--content", "third"]);
    let all = alice.wait_for(3);

    let contents = |messages: &[Value]| -> Vec<String> {
        messages
            .iter()
            .map(|m| m["content"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(contents(&all), ["first", "second", "third"]);
    assert_eq!(
        contents(&alice.messages(&["--limit", "2"])),
        ["second", "third"]
    );
    let second = all[1]["created_at"].as_u64().unwrap().to_string();
    assert_eq!(
        contents(&alice.messages(&["--since", &second])),
        ["second", "third"]
    );
    assert_eq!(
        contents(&alice.messages(&["--since", &second, "--limit", "1"])),
        ["third"]
    );
}

#[test]
fn without_a_kept_secret_listing_fails_and_a_secret_kept_later_is_picked_up() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let a = alice.identity();
    let key = alice.machine.agent_node_home().join("agent.key");
    fs::remove_file(&key).unwrap();

    let refused = alice.list(&[]);
    assert_eq!(refused.exit_code, 1, "{}", refused.stdout);
    let error = &refused.json()["error"];
    assert_eq!(error["code"], "agent_key_not_kept");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("toon event publish") && message.contains("toon message send"));

    // `event publish` keeps it, and the supervisor picks it up without a restart.
    let published = alice.toon(&[
        "event",
        "publish",
        "--kind",
        "1",
        "--content",
        "x",
        "--json",
    ]);
    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    assert!(key.exists());
    deliver(&bob, &a, &[], "found", &alice);
    let messages = alice.wait_for(1);
    assert_eq!(messages[0]["content"], "found");
}

#[test]
fn list_is_in_the_help() {
    let machine = Machine::new();
    let help = machine.toon(&["message", "--help"]);
    assert!(help.stdout.contains("list"), "{}", help.stdout);
}
