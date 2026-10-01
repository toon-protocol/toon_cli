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
    let address = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
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
    let body = json!({
        "name": "far",
        "toon": {
            "ilp_address": "g.toon.relay.far",
            "connector_url": far.url(),
            "price": PRICE,
        },
    })
    .to_string();
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
    let routed = near.toon(&["route", "add", "g.toon.relay.far", "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
}

#[test]
fn an_event_is_published_to_a_relay_behind_another_agent_node_and_read_back() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);

    let published = near.toon(&[
        "event",
        "publish",
        "--relay",
        &relay,
        "--kind",
        "1",
        "--content",
        "across",
        "--yes",
        "--json",
    ]);

    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    assert_eq!(report["paid"], PRICE);
    let event = report["event"].clone();

    let query = near.machine.toon(&[
        "event",
        "query",
        &far.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
        "--json",
    ]);
    assert_eq!(
        query.json()["events"],
        Value::Array(vec![event]),
        "{}",
        query.stdout
    );
}

#[test]
fn the_price_is_shown_and_nothing_is_paid_without_yes() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--json",
    ]);

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "not_confirmed", "{error}");
    assert!(error["message"].as_str().unwrap().contains("costs 1 base"));
    assert_eq!(run.exit_code, 1);
    let query = near.machine.toon(&[
        "event",
        "query",
        &far.relay_url(),
        "--filter",
        "{}",
        "--json",
    ]);
    assert_eq!(query.json()["events"], json!([]));
}

#[test]
fn a_price_over_the_spending_limit_is_refused() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let set = near.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "0",
        "--max-per-day",
        "10",
        "--json",
    ]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "spending_limit");
}

#[test]
fn with_no_peering_the_command_says_one_is_needed_and_creates_none() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "peering_needed", "{error}");
    assert_eq!(run.exit_code, 1);
    let peers = near.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(0));
}
