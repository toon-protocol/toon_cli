mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use serde_json::{json, Value};
use support::anvil_chain::AnvilChain;
use support::{Foreground, Machine, Run};

/// What the peering's channel is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

/// The price of a write the far relay states.
const PRICE: u64 = 1;

struct Node {
    machine: Machine,
    _up: Foreground,
    address: String,
}

impl Node {
    fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }

    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    fn relay_url(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"));
        format!("ws://{address}")
    }
}

fn node_on(chain: &AnvilChain) -> Node {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let report = up.report();
    let address = report["connector"]["address"]
        .as_str()
        .unwrap_or_else(|| panic!("the connector's address: {report}"))
        .to_owned();
    Node {
        machine,
        _up: up,
        address,
    }
}

/// A relay's information document, served for any request, naming `far`'s connector as
/// where a write is paid for. Returns the relay's `ws://` URL.
fn information_document(far: &Node) -> String {
    document(json!({
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": far.url(),
        "connector_seal_key": support::seal_key(&far.url()),
        "price": PRICE,
    }))
}

/// A relay's information document with `toon` as given, served for any request.
fn document(toon: Value) -> String {
    let body = json!({ "name": "far", "toon": toon }).to_string();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/nostr+json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("ws://{address}")
}

fn peer_and_route(near: &Node, far: &Node) {
    let peered = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--id",
        "far",
    ]);
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
    let routed = near.toon(&["route", "add", &far.machine.relay_prefix(), "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
}

const ALICE: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const BOB: &str = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";

/// The wraps the relay of `node` holds.
fn wraps_at(asker: &Node, relay: &str) -> Vec<Value> {
    let query = asker.machine.toon(&[
        "event",
        "query",
        relay,
        "--filter",
        r#"{"kinds":[1059]}"#,
        "--json",
    ]);
    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    query.json()["events"].as_array().unwrap().clone()
}

fn remaining_today(node: &Node) -> u128 {
    node.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .as_str()
        .expect("remaining_today")
        .parse()
        .expect("a number")
}

#[test]
fn the_recipients_wraps_are_paid_for_and_the_senders_copy_stays_home() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);

    let sent = near.toon(&[
        "message",
        "send",
        ALICE,
        BOB,
        "--content",
        "across",
        "--relay",
        &relay,
        "--yes",
        "--json",
    ]);

    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    let report = sent.json();
    assert_eq!(report["outcome"], "sent", "{report}");
    assert_eq!(report["paid"], PRICE * 2);
    let at_far = wraps_at(&near, &far.relay_url());
    let mut addressed: Vec<&str> = at_far
        .iter()
        .map(|wrap| wrap["tags"][0][1].as_str().unwrap())
        .collect();
    addressed.sort();
    assert_eq!(addressed, [ALICE, BOB]);
    // The sender's copy is on the own relay, and only that.
    let at_home = wraps_at(&near, &near.relay_url());
    assert_eq!(at_home.len(), 1, "{at_home:?}");
    let identity =
        near.toon(&["wallet", "show", "--json"]).json()["wallet"]["agent_identity"].clone();
    assert_eq!(at_home[0]["tags"][0][1], identity);
}

#[test]
fn without_yes_the_total_is_stated_and_nothing_is_sent_or_paid() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let before = remaining_today(&near);

    let run = near.toon(&[
        "message",
        "send",
        ALICE,
        BOB,
        "--content",
        "across",
        "--relay",
        &relay,
        "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "not_confirmed");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(&format!("{} base units", PRICE * 2)),
        "{error}"
    );
    assert_eq!(wraps_at(&near, &far.relay_url()), Vec::<Value>::new());
    assert_eq!(wraps_at(&near, &near.relay_url()), Vec::<Value>::new());
    assert_eq!(remaining_today(&near), before);
}

#[test]
fn a_total_is_counted_against_the_spending_limit() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let before = remaining_today(&near);

    let sent = near.toon(&[
        "message",
        "send",
        ALICE,
        BOB,
        "--content",
        "across",
        "--relay",
        &relay,
        "--yes",
        "--json",
    ]);
    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);

    assert_eq!(remaining_today(&near), before - u128::from(PRICE) * 2);
}

#[test]
fn a_total_past_the_limit_is_refused_and_nothing_is_written() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    // One wrap fits; two do not.
    let set = near.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "1",
        "--max-per-day",
        "10",
        "--json",
    ]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let run = near.toon(&[
        "message",
        "send",
        ALICE,
        BOB,
        "--content",
        "across",
        "--relay",
        &relay,
        "--yes",
        "--json",
    ]);

    assert_eq!(
        run.json()["error"]["code"],
        "spending_limit",
        "{}",
        run.stdout
    );
    assert_eq!(wraps_at(&near, &far.relay_url()), Vec::<Value>::new());
    assert_eq!(wraps_at(&near, &near.relay_url()), Vec::<Value>::new());
}
