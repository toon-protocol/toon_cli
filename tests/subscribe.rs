mod support;

use serde_json::json;
use support::anvil_chain::AnvilChain;
use support::fake_remote_relay::FakeRemoteRelay;
use support::{Foreground, Machine, Run};

/// What the peering's channel is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

/// What the fake relay's subscribe route charges a packet, and debits an event.
const PRICE: u64 = 1000;
const BROADCAST_PRICE: u64 = 10;
const SUBSCRIBE: &str = "g.toon.subscribe";

struct Node {
    machine: Machine,
    _up: Foreground,
}

impl Node {
    /// The connector's `/ilp` URL. A connector that has just been restarted by `add` is
    /// given a moment to answer.
    fn url(&self) -> String {
        for _ in 0..100 {
            let status = self.machine.toon(&["status", "--json"]).json();
            if let Some(address) =
                status["agent_node"]["toon_apps"][0]["connector"]["address"].as_str()
            {
                return format!("http://{address}/ilp");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the connector has no address");
    }

    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
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
    // The report says the supervisor is listening: a command before it finds no agent node.
    up.report();
    Node { machine, _up: up }
}

/// A relay that sells its feed, behind a node of its own: the node's connector delivers
/// the subscribe route to the fake.
fn remote(chain: &AnvilChain) -> (Node, FakeRemoteRelay) {
    let far = node_on(chain);
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE);
    let status = far.machine.toon(&["status", "--json"]).json();
    let toon_app = status["agent_node"]["toon_apps"][0]["name"]
        .as_str()
        .expect("a TOON app")
        .to_owned();
    let added = far.toon(&[
        "add",
        "subscribe",
        "--to",
        &toon_app,
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
    (far, relay)
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
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
}

const FILTER: &str = r#"{"kinds":[1]}"#;

fn subscribe(near: &Node, relay: &FakeRemoteRelay, extra: &[&str]) -> Run {
    let url = relay.url();
    let mut args = vec!["relay", "subscribe", &url[..], "--json"];
    args.extend_from_slice(extra);
    near.toon(&args)
}

#[test]
fn an_amount_is_paid_as_whole_packets_and_the_credit_is_reported() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    // 2500 buys two packets of 1000: the 500 over is not paid.
    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2500", "--yes"],
    );

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let report = run.json();
    assert_eq!(report["outcome"], "subscribed", "{report}");
    assert_eq!(report["packets"], 2);
    assert_eq!(report["paid"], 2000);
    assert_eq!(report["credited"], 2000);
    assert_eq!(report["balance"], 2000);
    assert_eq!(report["filter"], json!({"kinds":[1]}));
    let key = report["subscriber_key"].as_str().unwrap().to_owned();
    assert_eq!(relay.credited(), vec![key.clone(), key.clone()]);
    assert_eq!(relay.subscription(&key).unwrap().balance, 2000);
}

#[test]
fn the_prices_and_the_events_the_amount_buys_are_shown_and_nothing_is_paid_without_yes() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    let run = subscribe(&near, &relay, &["--filter", FILTER, "--amount", "3000"]);

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "not_confirmed", "{error}");
    assert_eq!(run.exit_code, 1);
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("charges 1000 per subscribe packet"),
        "{message}"
    );
    assert!(message.contains("10 for each event"), "{message}");
    assert!(message.contains("buy 300 events"), "{message}");
    assert_eq!(relay.posts(), 0);
}

#[test]
fn a_price_over_the_spending_limit_is_refused() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let set = near.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "1999",
        "--max-per-day",
        "100000",
        "--json",
    ]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2000", "--yes"],
    );

    assert_eq!(run.json()["error"]["code"], "spending_limit");
    assert_eq!(relay.posts(), 0);
}

#[test]
fn subscribing_again_tops_up_and_a_new_filter_replaces_the_old_one() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let first = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "1000", "--yes"],
    );
    assert_eq!(first.exit_code, 0, "{}{}", first.stdout, first.stderr);

    // A top-up with no filter keeps the one the relay holds.
    let topped = subscribe(&near, &relay, &["--amount", "1000", "--yes"]);
    assert_eq!(topped.exit_code, 0, "{}{}", topped.stdout, topped.stderr);
    assert_eq!(topped.json()["balance"], 2000);
    assert_eq!(topped.json()["filter"], json!({"kinds":[1]}));

    let replaced = subscribe(
        &near,
        &relay,
        &["--filter", r#"{"kinds":[7]}"#, "--amount", "1000", "--yes"],
    );
    assert_eq!(
        replaced.exit_code, 0,
        "{}{}",
        replaced.stdout, replaced.stderr
    );
    let report = replaced.json();
    assert_eq!(report["balance"], 3000, "{report}");
    assert_eq!(report["filter"], json!({"kinds":[7]}));
    let key = report["subscriber_key"].as_str().unwrap();
    let held = relay.subscription(key).unwrap();
    assert_eq!((held.balance, held.filter), (3000, json!({"kinds":[7]})));
}

#[test]
fn subscriptions_lists_the_balance_and_filter_at_each_relay() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let empty = near.toon(&["relay", "subscriptions", "--json"]);
    assert_eq!(empty.json()["subscriptions"], json!([]));
    let paid = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2000", "--yes"],
    );
    assert_eq!(paid.exit_code, 0, "{}{}", paid.stdout, paid.stderr);

    let listed = near.toon(&["relay", "subscriptions", "--json"]);

    assert_eq!(listed.exit_code, 0, "{}{}", listed.stdout, listed.stderr);
    let subscriptions = listed.json()["subscriptions"].clone();
    let key = paid.json()["subscriber_key"].clone();
    assert_eq!(
        subscriptions,
        json!([{
            "relay": relay.url(),
            "subscriber_key": key,
            "filter": {"kinds":[1]},
            "balance": 2000,
            "broadcast_price": BROADCAST_PRICE,
            "current": true,
        }])
    );
}

#[test]
fn the_subscriber_key_is_not_the_agent_identity() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let paid = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "1000", "--yes"],
    );
    assert_eq!(paid.exit_code, 0, "{}{}", paid.stdout, paid.stderr);

    let published = near.toon(&["event", "publish", "--kind", "1", "--json"]);

    let identity = published.json()["event"]["pubkey"].clone();
    assert_ne!(
        paid.json()["subscriber_key"],
        identity,
        "{}",
        published.stdout
    );
}

#[test]
fn with_no_peering_the_command_says_one_is_needed_with_its_deposit_and_creates_none() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (_far, relay) = remote(&chain);

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2500", "--yes"],
    );

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "peering_needed", "{error}");
    assert_eq!(run.exit_code, 1);
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("deposit of at least 2000"), "{message}");
    let peers = near.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(0));
    assert_eq!(relay.posts(), 0);
}

#[test]
fn a_first_subscription_needs_a_filter_and_an_amount_must_buy_a_packet() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    for extra in [
        &["--amount", "1000", "--yes"][..],
        &["--filter", FILTER, "--amount", "999", "--yes"][..],
        &["--filter", "[1]", "--amount", "1000", "--yes"][..],
    ] {
        let run = subscribe(&near, &relay, extra);
        assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
        assert_eq!(run.exit_code, 2);
    }
    assert_eq!(relay.posts(), 0);
}
