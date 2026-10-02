//! A relay at a `wss://` URL: the stored events, the information document, the live feed
//! and the receiver, against the fake remote relay serving TLS with a certificate made for
//! the test and handed over as `TOON_TRUSTED_ROOT`.

mod support;

use serde_json::{json, Value};
use support::anvil_chain::AnvilChain;
use support::fake_remote_relay::FakeRemoteRelay;
use support::{Foreground, Machine, Run};

const DEPOSIT: u128 = 1_000_000;
const PRICE: u64 = 1000;
const BROADCAST_PRICE: u64 = 10;
const SUBSCRIBE: &str = "g.toon.subscribe";
const FILTER: &str = r#"{"kinds":[1]}"#;

fn event(number: u64) -> Value {
    json!({
        "id": format!("{number:064x}"),
        "pubkey": "ab".repeat(32),
        "created_at": 1_790_000_000 + number,
        "kind": 1,
        "tags": [],
        "content": format!("event {number}"),
        "sig": "00".repeat(64),
    })
}

fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("what was waited for did not happen");
}

fn query(machine: &Machine, url: &str, root: Option<&std::path::Path>) -> Run {
    machine.toon_with(
        &["event", "query", url, "--filter", FILTER, "--json"],
        |command| {
            if let Some(root) = root {
                command.env("TOON_TRUSTED_ROOT", root);
            }
        },
    )
}

#[test]
fn query_of_a_wss_relay_returns_the_stored_events_when_its_root_is_trusted() {
    let machine = Machine::new();
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE).with_tls(&[]);
    relay.broadcast(event(1));
    relay.broadcast(event(2));

    let run = query(&machine, &relay.wss_url(), Some(&relay.root_file()));

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["events"], json!([event(1), event(2)]));
    assert_eq!(run.json()["relay"], relay.wss_url());
}

#[test]
fn query_of_a_wss_relay_whose_certificate_is_not_trusted_names_the_certificate() {
    let machine = Machine::new();
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE).with_tls(&[]);

    let run = query(&machine, &relay.wss_url(), None);

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "query_failed");
    assert!(
        run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("certificate"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_trusted_root_that_is_missing_or_empty_fails_the_command() {
    let machine = Machine::new();
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE).with_tls(&[]);
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty.pem");
    std::fs::write(&empty, "").unwrap();

    for root in [directory.path().join("missing.pem"), empty] {
        let run = query(&machine, &relay.wss_url(), Some(&root));

        assert_eq!(
            run.json()["error"]["code"],
            "query_failed",
            "{}",
            run.stdout
        );
        assert!(run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("TOON_TRUSTED_ROOT"));
    }
}

#[test]
fn a_relay_url_of_another_scheme_is_refused_naming_both_schemes() {
    let machine = Machine::new();

    for url in [
        "http://relay.example",
        "https://relay.example",
        "relay.example",
    ] {
        let run = query(&machine, url, None);

        assert_eq!(run.json()["error"]["code"], "query_failed");
        let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(
            message.contains("ws://") && message.contains("wss://"),
            "{message}"
        );
    }
}

struct Node {
    machine: Machine,
    _up: Foreground,
}

impl Node {
    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    fn status(&self) -> Value {
        self.machine.toon(&["status", "--json"]).json()
    }

    fn url(&self) -> String {
        for _ in 0..100 {
            if let Some(address) =
                self.status()["agent_node"]["toon_apps"][0]["connector"]["address"].as_str()
            {
                return format!("http://{address}/ilp");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the connector has no address");
    }

    fn own_relay(&self) -> String {
        let status = self.status();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"));
        format!("ws://{address}")
    }

    fn stored(&self) -> Vec<String> {
        let query = self.machine.toon(&[
            "event",
            "query",
            &self.own_relay(),
            "--filter",
            FILTER,
            "--json",
        ]);
        query.json()["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A running agent node on `chain`; the supervisor trusts `root`, if given.
fn node_on(chain: &AnvilChain, root: Option<&std::path::Path>) -> Node {
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
    let up = machine.start_with(&["up", "--foreground", "--json"], |command| {
        if let Some(root) = root {
            command.env("TOON_TRUSTED_ROOT", root);
        }
    });
    up.report();
    Node { machine, _up: up }
}

#[test]
fn a_wss_relay_is_subscribed_followed_and_received_from() {
    let chain = AnvilChain::start();
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE).with_tls(&[]);
    let root = relay.root_file();
    let near = node_on(&chain, Some(&root));
    let far = node_on(&chain, None);

    let app = far.status()["agent_node"]["toon_apps"][0]["name"]
        .as_str()
        .expect("a TOON app")
        .to_owned();
    let added = far.toon(&[
        "add",
        "subscribe",
        "--to",
        &app,
        "--url",
        &relay.app_url(),
        "--address",
        SUBSCRIBE,
        "--price",
        &PRICE.to_string(),
        "--yes",
    ]);
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    relay.set_connector(&far.url());
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
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);

    let url = relay.wss_url();
    let subscribe = |trusting: bool| {
        near.machine.toon_with(
            &[
                "relay",
                "subscribe",
                &url,
                "--filter",
                FILTER,
                "--amount",
                "1000",
                "--yes",
                "--json",
            ],
            |command| {
                command.env("TOON_PASSPHRASE", support::PASSPHRASE);
                if trusting {
                    command.env("TOON_TRUSTED_ROOT", &root);
                }
            },
        )
    };

    // Without the root the information document cannot be read, and nothing is paid.
    let refused = subscribe(false);
    assert_eq!(refused.json()["error"]["code"], "relay_not_payable");
    assert!(refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("certificate"));
    assert_eq!(relay.posts(), 0);

    let subscribed = subscribe(true);
    assert_eq!(
        subscribed.exit_code, 0,
        "{}{}",
        subscribed.stdout, subscribed.stderr
    );
    assert_eq!(subscribed.json()["outcome"], "subscribed");
    assert_eq!(subscribed.json()["relay"], url);

    // The supervisor's receiver holds the subscription at the `wss://` URL as given.
    eventually(|| relay.open_feeds() == 1);
    let follow = near
        .machine
        .start_with(&["event", "follow", &url, "--json"], |command| {
            command.env("TOON_TRUSTED_ROOT", &root);
        });
    eventually(|| relay.open_feeds() == 2);
    relay.broadcast(event(7));

    let printed: Value = serde_json::from_str(&follow.line()).expect("one document");
    assert_eq!(printed, event(7));
    // The receiver delivered it to the agent node's own relay.
    eventually(|| near.stored() == vec![event(7)["id"].as_str().unwrap().to_owned()]);
}
