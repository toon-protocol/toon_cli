//! The agent node's own relay serves a gift wrap only to the key it is addressed to, and
//! the relay is always started so.

mod support;

use std::fs;

use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::Machine;

fn relay_url(machine: &Machine) -> String {
    let status = machine.toon(&["status", "--json"]).json();
    let address = status["agent_node"]["toon_apps"][0]["apps"][0]["read_address"]
        .as_str()
        .unwrap_or_else(|| panic!("the relay has no read address: {status}"));
    format!("ws://{address}")
}

fn names(machine: &Machine) -> String {
    fs::read_to_string(machine.agent_node_home().join("apps/relay/data/names")).unwrap()
}

fn query(machine: &Machine, filter: &Value) -> support::Run {
    machine.toon(&[
        "event",
        "query",
        &relay_url(machine),
        "--filter",
        &filter.to_string(),
        "--json",
    ])
}

#[test]
fn the_relay_is_started_with_recipient_only_wraps_with_and_without_a_sold_feed() {
    for sells in [false, true] {
        let chain = FakeChain::start();
        let machine = Machine::new();
        machine.init_on(&chain);
        if sells {
            let run = machine.toon(&[
                "relay",
                "price",
                "--subscribe",
                "1000",
                "--broadcast",
                "10",
                "--json",
            ]);
            assert_eq!(run.exit_code, 0, "{}", run.stdout);
        }
        let up = machine.start(&["up", "--foreground", "--json"]);
        up.report();
        let names = names(&machine);
        assert!(
            names
                .lines()
                .any(|name| name == "TOON_NIP17_RECIPIENT_ONLY"),
            "{names}"
        );
        // The agent identity is never made an operator to read its wraps.
        assert!(
            !names.lines().any(|name| name == "TOON_OPERATOR_PUBKEYS"),
            "{names}"
        );
    }
}

#[test]
fn a_query_for_wraps_is_refused_and_one_for_notes_is_not() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let wraps = query(&machine, &json!({ "kinds": [1059] }));
    assert_ne!(wraps.exit_code, 0, "{}", wraps.stdout);
    assert!(
        format!("{}{}", wraps.stdout, wraps.stderr).contains("auth-required:"),
        "{}{}",
        wraps.stdout,
        wraps.stderr
    );

    let notes = query(&machine, &json!({ "kinds": [1] }));
    assert_eq!(notes.exit_code, 0, "{}{}", notes.stdout, notes.stderr);
}

/// What the relay at `url` sends a connection that proves `secret` and then asks with
/// `filter`, up to its `EOSE` or `CLOSED`.
fn read_as(url: &str, secret: &[u8; 32], filter: &Value) -> Vec<Value> {
    use k256::schnorr::SigningKey;
    use sha2::{Digest, Sha256};

    let (mut socket, _) = tungstenite::connect(url).expect("the relay");
    let challenge = loop {
        let frame: Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).expect("a frame");
        if frame[0] == "AUTH" {
            break frame[1].as_str().unwrap().to_owned();
        }
    };
    let key = SigningKey::from_bytes(secret).unwrap();
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let (created_at, kind, content) = (1_700_000_000_u64, 22242, "");
    let tags = json!([["relay", url], ["challenge", challenge]]);
    let id: [u8; 32] =
        Sha256::digest(json!([0, pubkey, created_at, kind, tags, content]).to_string()).into();
    let sig = key.sign_raw(&id, &[0; 32]).unwrap();
    let auth = json!({
        "id": hex::encode(id),
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
        "sig": hex::encode(sig.to_bytes()),
    });
    for message in [json!(["AUTH", auth]), json!(["REQ", "read", filter])] {
        socket
            .send(tungstenite::Message::text(message.to_string()))
            .unwrap();
    }
    let mut events = Vec::new();
    loop {
        let frame: Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).expect("a frame");
        match frame[0].as_str() {
            Some("OK") => assert_eq!(frame[2], true, "{frame}"),
            Some("EVENT") => events.push(frame[2].clone()),
            Some("EOSE" | "CLOSED") => return events,
            _ => {}
        }
    }
}

#[test]
fn a_reader_proving_another_key_gets_no_wrap_for_the_agent_identity() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    // The sender's copy of a message is a wrap addressed to the agent identity.
    let other = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
    let sent = machine.toon_with(
        &["message", "send", other, "--content", "hello", "--json"],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );
    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert!(!machine.stored_events(&[1059]).is_empty());

    let url = relay_url(&machine);
    for filter in [json!({ "kinds": [1059] }), json!({})] {
        let events = read_as(&url, &[7; 32], &filter);
        assert!(
            events.iter().all(|event| event["kind"] != 1059),
            "{filter}: {events:?}"
        );
    }
}
