//! An agent key publishes events and sends private messages as the agent identity without
//! holding its secret: the supervisor signs (ADR 0010), the agent pays.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

/// An agent node that is up, and a home of nobody's for the agent to run in.
struct Node {
    machine: Machine,
    nowhere: tempfile::TempDir,
    up: Foreground,
}

fn node_on(chain: &FakeChain) -> Node {
    let machine = Machine::new();
    let init = machine.init_on(chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Node {
        machine,
        nowhere: tempfile::tempdir().expect("a directory"),
        up,
    }
}

impl Node {
    fn toon(&self, args: &[&str]) -> Run {
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

    /// An agent key's file.
    fn create(&self, extra: &[&str]) -> PathBuf {
        let mut args = vec!["agent-key", "create", "--json"];
        args.extend_from_slice(extra);
        let run = self.toon(&args);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        PathBuf::from(run.json()["key_file"].as_str().expect("key file"))
    }

    /// `toon` as the agent runs it: no agent node in `HOME`, the key file in the environment.
    fn agent(&self, key: &Path, args: &[&str]) -> Run {
        // The supervisor binds its order address a moment after the key is made.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let run = self.machine.toon_with(args, |command| {
                command
                    .env("HOME", self.nowhere.path())
                    .env("TOON_AGENT_KEY", key);
            });
            if run.exit_code != 1
                || !run.stdout.contains("did not answer")
                || Instant::now() > deadline
            {
                return run;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn relay(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"))
            .to_owned()
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

    fn wraps_for(&self, key: &str) -> Vec<Value> {
        let mut wraps = self.machine.stored_events(&[1059]);
        wraps.retain(|wrap| {
            wrap["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag[0] == "p" && tag[1] == key))
        });
        wraps
    }

    /// The messages `toon message list` prints with the home, once there are `count`.
    fn wait_for(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let run = self.machine.toon(&["message", "list", "--json"]);
            assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
            let messages = run.json()["messages"].as_array().unwrap().clone();
            if messages.len() >= count {
                return messages;
            }
            assert!(
                Instant::now() < deadline,
                "{count} messages were not opened: {messages:?}\n{}",
                self.up.stderr()
            );
            thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Whether `event` carries a signature that verifies against its own `pubkey`.
fn verifies(event: &Value) -> bool {
    let id = hex::decode(event["id"].as_str().unwrap()).unwrap();
    let key =
        VerifyingKey::from_bytes(&hex::decode(event["pubkey"].as_str().unwrap()).unwrap()).unwrap();
    let signature = Signature::try_from(
        hex::decode(event["sig"].as_str().unwrap())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    key.verify_prehash(&id, &signature).is_ok()
}

#[test]
fn a_published_event_is_signed_by_the_agent_identity_and_paid_by_the_agent_key() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let key = node.create(&["--max-per-command", "5", "--max-per-day", "100"]);
    let identity = node.identity();

    let published = node.agent(
        &key,
        &[
            "event",
            "publish",
            "--kind",
            "1",
            "--content",
            "from the agent",
            "--tags",
            r#"[["t","toon"]]"#,
            "--amount",
            "3",
            "--json",
        ],
    );
    assert_eq!(
        published.exit_code, 0,
        "{}{}",
        published.stdout, published.stderr
    );
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    let event = &report["event"];
    assert_eq!(event["pubkey"], identity);
    assert_eq!(event["content"], "from the agent");
    assert!(verifies(event), "{event}");
    assert_eq!(node.machine.stored_events(&[1]), vec![event.clone()]);
    assert!(!node.nowhere.path().join(".toon").exists());

    // The connector counted the packet against the agent key, not the operator's own.
    let limit = node.agent(&key, &["limit", "show", "--json"]);
    assert_eq!(limit.json()["limits"]["relay"]["remaining_today"], "97");

    // The secret is in no answer.
    let secret = hex::encode(fs::read(node.machine.agent_node_home().join("agent.key")).unwrap());
    for output in [&published.stdout, &published.stderr, &limit.stdout] {
        assert!(!output.contains(&secret));
    }
}

#[test]
fn a_publish_the_connector_does_not_let_the_key_send_is_refused() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let key = node.create(&[
        "--scope",
        "sign",
        "--max-per-command",
        "5",
        "--max-per-day",
        "100",
    ]);
    let run = node.agent(
        &key,
        &[
            "event",
            "publish",
            "--kind",
            "1",
            "--content",
            "x",
            "--json",
        ],
    );
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert!(node.machine.stored_events(&[1]).is_empty());
}

#[test]
fn a_signed_event_is_printed_and_no_packet_is_sent() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    // No `send`: a packet would be refused, so success shows none was sent.
    let key = node.create(&["--scope", "sign"]);

    let signed = node.agent(
        &key,
        &[
            "event",
            "sign",
            "--kind",
            "30023",
            "--content",
            "draft",
            "--json",
        ],
    );
    assert_eq!(signed.exit_code, 0, "{}{}", signed.stdout, signed.stderr);
    let event = signed.json()["event"].clone();
    assert_eq!(event["pubkey"], node.identity());
    assert_eq!(event["kind"], 30023);
    assert!(verifies(&event), "{event}");
    assert!(node.machine.stored_events(&[30023]).is_empty());

    let text = node.agent(&key, &["event", "sign", "--kind", "1"]);
    assert_eq!(text.exit_code, 0, "{}{}", text.stdout, text.stderr);
    let printed: Value = serde_json::from_str(text.stdout.trim()).expect("the event alone");
    assert!(verifies(&printed));
}

#[test]
fn a_key_without_sign_or_read_messages_is_refused_those_orders() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let key = node.create(&["--scope", "status"]);
    let bob = "7".repeat(64);

    for args in [
        vec!["event", "sign", "--kind", "1", "--json"],
        vec!["event", "publish", "--kind", "1", "--json"],
        vec!["message", "send", &bob, "--content", "hi", "--json"],
        vec!["message", "list", "--json"],
    ] {
        let run = node.agent(&key, &args);
        assert_eq!(run.exit_code, 1, "{args:?}: {}{}", run.stdout, run.stderr);
        let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains("403"), "{args:?}: {message}");
    }
    assert!(node.machine.stored_events(&[1, 1059]).is_empty());

    // Each scope is its own: `sign` does not read messages, nor the reverse.
    let signs = node.create(&["--scope", "sign"]);
    let list = node.agent(&signs, &["message", "list", "--json"]);
    assert!(list.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("read-messages"));
    let reads = node.create(&["--scope", "read-messages"]);
    let sign = node.agent(&reads, &["event", "sign", "--kind", "1", "--json"]);
    assert!(sign.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("'sign'"));
}

#[test]
fn a_private_message_sent_with_an_agent_key_can_be_opened_by_its_recipient() {
    let chain = FakeChain::start();
    let (alice, bob) = (node_on(&chain), node_on(&chain));
    let key = alice.create(&[]);
    let (a, b) = (alice.identity(), bob.identity());

    let sent = alice.agent(
        &key,
        &["message", "send", &b, "--content", "psst", "--json"],
    );
    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert_eq!(sent.json()["outcome"], "sent", "{}", sent.stdout);

    // Alice's relay holds the sender's copy and the recipient's wrap; bob's relay is written
    // the recipient's wrap as a subscription would.
    assert_eq!(alice.wraps_for(&a).len(), 1);
    let wraps = alice.wraps_for(&b);
    assert_eq!(wraps.len(), 1);
    for wrap in &wraps {
        assert!(verifies(wrap));
        bob.write(wrap);
    }
    let received = bob.wait_for(1);
    assert_eq!(received[0]["content"], "psst");
    assert_eq!(received[0]["from"], a);

    // The sender's copy is listed with the key as it is with the home.
    let with_home = alice.wait_for(1);
    let listed = alice.agent(&key, &["message", "list", "--json"]);
    assert_eq!(listed.exit_code, 0, "{}{}", listed.stdout, listed.stderr);
    assert_eq!(listed.json()["messages"], Value::Array(with_home));
    let home_text = alice.machine.toon(&["message", "list"]).stdout;
    assert_eq!(
        alice.agent(&key, &["message", "list"]).stdout,
        home_text,
        "the same text"
    );
}

#[test]
fn the_same_commands_with_the_home_behave_as_before() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let signed = node.toon(&["event", "sign", "--kind", "1", "--json"]);
    assert_eq!(signed.exit_code, 0, "{}{}", signed.stdout, signed.stderr);
    assert!(verifies(&signed.json()["event"]));
    let published = node.toon(&["event", "publish", "--kind", "1", "--json"]);
    assert_eq!(published.json()["outcome"], "published");
}

#[test]
fn creating_a_signing_key_keeps_the_secret_for_the_supervisor() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let kept = node.machine.agent_node_home().join("agent.key");
    fs::remove_file(&kept).unwrap();
    let key = node.create(&["--scope", "sign"]);
    assert!(kept.exists());
    let signed = node.agent(&key, &["event", "sign", "--kind", "1", "--json"]);
    assert_eq!(signed.exit_code, 0, "{}{}", signed.stdout, signed.stderr);
}

#[test]
fn signing_without_a_kept_secret_says_so() {
    let chain = FakeChain::start();
    let node = node_on(&chain);
    let key = node.create(&["--scope", "sign"]);
    fs::remove_file(node.machine.agent_node_home().join("agent.key")).unwrap();
    let run = node.agent(&key, &["event", "sign", "--kind", "1", "--json"]);
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "agent_key_not_kept");
}
