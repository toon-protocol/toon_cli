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
    up: Option<Foreground>,
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

impl Node {
    /// Stop the agent node and start it again, as a restart of the machine would.
    fn restart(&mut self) {
        let down = self.machine.toon(&["down", "--json"]);
        assert_eq!(down.exit_code, 0, "{}{}", down.stdout, down.stderr);
        self.up = None;
        let up = self.machine.start(&["up", "--foreground", "--json"]);
        up.report();
        self.up = Some(up);
    }

    /// The websocket URL of this agent node's own relay: the fake serves it on its write port.
    fn own_relay(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"));
        format!("ws://{address}")
    }

    /// The ids of the kind 1 events that can be read from this agent node's own relay.
    fn stored(&self) -> Vec<String> {
        let query = self.machine.toon(&[
            "event",
            "query",
            &self.own_relay(),
            "--filter",
            r#"{"kinds":[1]}"#,
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

    /// Wait until the own relay holds the event `id`.
    fn wait_for_stored(&self, id: &str) {
        eventually(|| self.stored().iter().any(|stored| stored == id));
    }
}

/// Wait for `condition` to hold, for as long as a slow machine may need.
fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("what was waited for did not happen");
}

/// An event of `kind`, as a relay broadcasts it. Nothing here checks a signature: the
/// fake relays do not.
fn event(number: u64, kind: u64) -> serde_json::Value {
    json!({
        "id": format!("{number:064x}"),
        "pubkey": "ab".repeat(32),
        "created_at": 1_790_000_000 + number,
        "kind": kind,
        "tags": [],
        "content": format!("event {number}"),
        "sig": "00".repeat(64),
    })
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
    Node {
        machine,
        up: Some(up),
    }
}

/// A relay that sells its feed, behind a node of its own: the node's connector delivers
/// the subscribe route to the fake.
fn remote(chain: &AnvilChain) -> (Node, FakeRemoteRelay) {
    remote_debiting(chain, BROADCAST_PRICE)
}

/// Like `remote`, for a relay that debits `broadcast_price` for an event.
fn remote_debiting(chain: &AnvilChain, broadcast_price: u64) -> (Node, FakeRemoteRelay) {
    let far = node_on(chain);
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, broadcast_price);
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
fn a_first_packet_that_fails_before_it_is_sent_is_not_counted() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    // The relay pins a key that reads as one and is no point on the curve, so the packet
    // cannot be sealed.
    relay.publish_connector(&far.url(), support::UNSEALABLE_KEY);
    let remaining =
        || near.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"].clone();
    let before = remaining();

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2000", "--yes"],
    );

    assert_eq!(run.json()["error"]["code"], "send_failed", "{}", run.stdout);
    assert_eq!(run.exit_code, 1);
    assert_eq!(remaining(), before);
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
    assert_eq!(
        empty.json()["totals"],
        json!({ "active": 0, "exhausted": 0 })
    );
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
            "exhausted": false,
        }])
    );
    assert_eq!(
        listed.json()["totals"],
        json!({ "active": 1, "exhausted": 0 })
    );
    assert!(near
        .toon(&["relay", "subscriptions"])
        .stdout
        .contains("1 with a balance, 0 exhausted."));
    let status = near.machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["totals"]["subscriptions"],
        json!({ "active": 1, "exhausted": 0 })
    );
}

#[test]
fn a_subscription_the_relay_forgot_lists_as_empty_and_a_top_up_opens_it_again() {
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
    let key = paid.json()["subscriber_key"].as_str().unwrap().to_owned();
    relay.forget(&key);

    let listed = near.toon(&["relay", "subscriptions", "--json"]).json();
    assert_eq!(listed["subscriptions"][0]["balance"], 0, "{listed}");
    assert_eq!(listed["subscriptions"][0]["current"], true);

    // The top-up sends the filter last kept, so the relay opens the subscription again.
    let topped = subscribe(&near, &relay, &["--amount", "1000", "--yes"]);
    assert_eq!(topped.exit_code, 0, "{}{}", topped.stdout, topped.stderr);
    let held = relay.subscription(&key).unwrap();
    assert_eq!((held.balance, held.filter), (1000, json!({"kinds":[1]})));
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
    let (far, relay) = remote(&chain);

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
    assert!(
        message.contains(&format!(
            "toon peer add {} --deposit 2000 --yes`",
            far.url()
        )),
        "{message}"
    );
    let peers = near.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(0));
    assert_eq!(relay.posts(), 0);
}

#[test]
fn with_no_peering_the_deposit_named_is_what_would_be_paid_not_what_is_credited() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (_far, relay) = remote(&chain);

    let run = subscribe(
        &near,
        &relay,
        &[
            "--filter",
            FILTER,
            "--amount",
            "2500",
            "--packet-amount",
            "1100",
            "--yes",
        ],
    );

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "peering_needed", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("deposit of at least 2200"), "{message}");
    assert!(message.contains("--deposit 2200 --yes`"), "{message}");
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

fn subscribed(near: &Node, relay: &FakeRemoteRelay, amount: &str) -> serde_json::Value {
    let run = subscribe(
        near,
        relay,
        &["--filter", FILTER, "--amount", amount, "--yes"],
    );
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    run.json()
}

#[test]
fn an_event_of_the_feed_is_handed_to_the_own_relay_and_read_from_it() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let paid = subscribed(&near, &relay, "1000");
    let key = paid["subscriber_key"].as_str().unwrap();
    eventually(|| relay.open_feeds() == 1);

    relay.broadcast(event(1, 7));
    relay.broadcast(event(2, 1));

    // Only what the subscription's filter asks for is sent, and only that is paid for.
    near.wait_for_stored(event(2, 1)["id"].as_str().unwrap());
    assert_eq!(near.stored(), vec![event(2, 1)["id"].as_str().unwrap()]);
    assert_eq!(
        relay.subscription(key).unwrap().balance,
        1000 - BROADCAST_PRICE
    );
}

#[test]
fn every_subscription_resumes_after_the_agent_node_restarts() {
    let chain = AnvilChain::start();
    let mut near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    subscribed(&near, &relay, "1000");
    eventually(|| relay.open_feeds() == 1);

    near.restart();
    eventually(|| relay.open_feeds() == 1);
    relay.broadcast(event(3, 1));

    near.wait_for_stored(event(3, 1)["id"].as_str().unwrap());
}

#[test]
fn a_subscription_that_runs_out_is_said_so_and_resumes_when_it_is_topped_up() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    // 1000 buys one event at 600, and 400 is left, which buys none.
    let (far, relay) = remote_debiting(&chain, 600);
    peer_and_route(&near, &far);
    subscribed(&near, &relay, "1000");
    eventually(|| relay.open_feeds() == 1);

    relay.broadcast(event(4, 1));
    relay.broadcast(event(5, 1));

    near.wait_for_stored(event(4, 1)["id"].as_str().unwrap());
    // The supervisor notes that the relay closed the feed.
    eventually(|| {
        let status = near.machine.toon(&["status", "--json"]).json();
        status["agent_node"]["subscriptions"][0]["exhausted"] == true
    });
    let status = near.machine.toon(&["status"]);
    assert!(
        status
            .stdout
            .contains(&format!("Subscription at {}: exhausted", relay.url())),
        "{}",
        status.stdout
    );
    let listed = near.toon(&["relay", "subscriptions", "--json"]);
    assert_eq!(listed.json()["subscriptions"][0]["exhausted"], true);
    assert_eq!(listed.json()["subscriptions"][0]["balance"], 400);
    assert_eq!(
        listed.json()["totals"],
        json!({ "active": 0, "exhausted": 1 })
    );
    let text = near.toon(&["relay", "subscriptions"]);
    assert!(text.stdout.contains("exhausted"), "{}", text.stdout);
    assert_eq!(near.stored(), vec![event(4, 1)["id"].as_str().unwrap()]);

    // 1400 buys two more events: the feed is dialled again.
    subscribed(&near, &relay, "1000");
    eventually(|| relay.open_feeds() == 1);
    relay.broadcast(event(6, 1));
    near.wait_for_stored(event(6, 1)["id"].as_str().unwrap());
}

#[test]
fn follow_prints_each_event_as_it_arrives_one_json_document_to_a_line() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    subscribed(&near, &relay, "1000");
    // The supervisor's feed is one; the one `follow` opens is the other.
    eventually(|| relay.open_feeds() == 1);
    let url = relay.url();
    let follow = near.machine.start(&["event", "follow", &url, "--json"]);
    eventually(|| relay.open_feeds() == 2);

    relay.broadcast(event(7, 1));
    relay.broadcast(event(8, 1));

    let first: serde_json::Value = serde_json::from_str(&follow.line()).expect("one document");
    let second: serde_json::Value = serde_json::from_str(&follow.line()).expect("one document");
    assert_eq!((first, second), (event(7, 1), event(8, 1)));
}

#[test]
fn follow_needs_a_subscription() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (_far, relay) = remote(&chain);

    let run = near
        .machine
        .toon(&["event", "follow", &relay.url(), "--json"]);

    assert_eq!(
        run.json()["error"]["code"],
        "not_subscribed",
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
}

#[test]
fn a_subscription_is_sealed_to_the_published_key_and_never_dials_the_connector_url() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);
    let hint = support::spy::start();
    relay.publish_connector(&hint.url(), &support::seal_key(&far.url()));

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "1000", "--yes"],
    );

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["outcome"], "subscribed");
    assert_eq!(hint.connections(), 0);
}

#[test]
fn a_relay_without_a_whole_seal_key_is_not_payable() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    // An empty key is left out of the document; the last is 65 bytes but not uncompressed.
    let compressed = format!("05{}", "ab".repeat(64));
    for key in ["", "04ab", "not hex", &compressed] {
        relay.publish_connector(&far.url(), key);
        let run = subscribe(
            &near,
            &relay,
            &["--filter", FILTER, "--amount", "1000", "--yes"],
        );
        let error = run.json()["error"].clone();
        assert_eq!(error["code"], "relay_not_payable", "{key}: {error}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("`connector_seal_key`"),
            "{key}: {error}"
        );
        assert_eq!(run.exit_code, 1);
    }
    assert_eq!(relay.posts(), 0);
}

/// What `mid` charges to forward a subscribe packet to the relay.
const FORWARD: u64 = 1100;

fn peer_with(from: &Node, to: &Node, id: &str) {
    let peered = from.toon(&[
        "peer",
        "add",
        &to.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--id",
        id,
    ]);
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
}

/// `near` peered with `mid`, `mid` peered with `far` and charging `FORWARD` for the
/// subscribe route, which `near` reaches through `mid`.
fn through_a_charging_connector(chain: &AnvilChain) -> (Node, Node, Node, FakeRemoteRelay) {
    let near = node_on(chain);
    let mid = node_on(chain);
    let (far, relay) = remote(chain);
    peer_with(&near, &mid, "mid");
    peer_with(&mid, &far, "far");
    let routed = mid.toon(&[
        "route",
        "add",
        SUBSCRIBE,
        "--peer",
        "far",
        "--price",
        &FORWARD.to_string(),
    ]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "mid"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    (near, mid, far, relay)
}

fn remaining(near: &Node) -> u128 {
    near.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .as_str()
        .expect("remaining_today")
        .parse()
        .expect("a number")
}

#[test]
fn a_connector_that_charges_to_forward_rejects_the_price_and_the_text_names_the_flag() {
    let chain = AnvilChain::start();
    let (near, _mid, _far, relay) = through_a_charging_connector(&chain);
    let before = remaining(&near);
    // The text report, then the JSON one: each way of looking at the rejection has its own run.
    let url = relay.url();
    let run = near.toon(&[
        "relay",
        "subscribe",
        &url,
        "--filter",
        FILTER,
        "--amount",
        "2200",
        "--yes",
    ]);
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    let text = format!("{}{}", run.stdout, run.stderr);
    assert!(text.contains("F03"), "{text}");
    assert!(text.contains("--packet-amount"), "{text}");
    // The rejected packet moved the channel by its amount, and the limit counts it.
    assert_eq!(remaining(&near), before - u128::from(PRICE));
}

#[test]
fn the_reject_of_a_charging_connector_is_in_the_json_report_unchanged() {
    let chain = AnvilChain::start();
    let (near, _mid, _far, relay) = through_a_charging_connector(&chain);

    let rejected = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2200", "--yes"],
    );

    assert_eq!(rejected.exit_code, 1, "{}", rejected.stdout);
    let report = rejected.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["response"]["code"], "F03", "{report}");
    assert_eq!(report["paid"], PRICE);
    assert_eq!(report["credited"], 0);
    assert_eq!(report["packet_amount"], PRICE);
}

#[test]
fn a_stated_packet_amount_pays_a_connector_that_charges_to_forward() {
    let chain = AnvilChain::start();
    let (near, _mid, _far, relay) = through_a_charging_connector(&chain);
    let before = remaining(&near);

    let run = subscribe(
        &near,
        &relay,
        &[
            "--filter",
            FILTER,
            "--amount",
            "2500",
            "--packet-amount",
            "1100",
            "--yes",
        ],
    );

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let report = run.json();
    assert_eq!(report["outcome"], "subscribed", "{report}");
    assert_eq!(report["packets"], 2);
    assert_eq!(report["paid"], 2200);
    assert_eq!(report["credited"], 2000);
    assert_eq!(report["price"], PRICE);
    assert_eq!(report["packet_amount"], 1100);
    let key = report["subscriber_key"].as_str().unwrap().to_owned();
    assert_eq!(relay.subscription(&key).unwrap().balance, 2000);
    assert_eq!(remaining(&near), before - 2200);
}

#[test]
fn without_yes_the_packets_the_amounts_and_the_credit_are_stated() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    let run = subscribe(
        &near,
        &relay,
        &[
            "--filter",
            FILTER,
            "--amount",
            "2500",
            "--packet-amount",
            "1100",
        ],
    );

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "not_confirmed", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("2 packets of 1100"), "{message}");
    assert!(message.contains("2200 base units"), "{message}");
    assert!(message.contains("2000 is credited"), "{message}");
    assert!(message.contains("200 events"), "{message}");
    assert_eq!(relay.posts(), 0);
}

#[test]
fn a_packet_amount_under_the_price_or_over_the_amount_is_usage_and_a_total_over_the_limit_is_refused(
) {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    for extra in [
        &["--packet-amount", "999", "--amount", "5000"][..],
        &["--packet-amount", "1100", "--amount", "1099"][..],
    ] {
        let mut args = vec!["--filter", FILTER, "--yes"];
        args.extend_from_slice(extra);
        let run = subscribe(&near, &relay, &args);
        assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
        assert_eq!(run.exit_code, 2);
        // Both messages name the packet amount the command was given.
        let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains(extra[1]), "{message}");
    }
    let limit = near.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "2199",
        "--max-per-day",
        "100000",
        "--json",
    ]);
    assert_eq!(limit.exit_code, 0, "{}{}", limit.stdout, limit.stderr);
    // Two packets of 1100 are 2200, over the limit of 2199.
    let run = subscribe(
        &near,
        &relay,
        &[
            "--filter",
            FILTER,
            "--packet-amount",
            "1100",
            "--amount",
            "2200",
            "--yes",
        ],
    );
    assert_eq!(
        run.json()["error"]["code"],
        "spending_limit",
        "{}",
        run.stdout
    );
    assert_eq!(relay.posts(), 0);
}

#[test]
fn a_direct_subscription_reports_the_packet_amount_as_the_price() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let (far, relay) = remote(&chain);
    peer_and_route(&near, &far);

    let report = subscribed(&near, &relay, "1000");

    assert_eq!(report["packet_amount"], PRICE);
    assert_eq!(report["price"], PRICE);
}

#[test]
fn a_rejected_packet_leaves_nothing_behind_for_the_next_run() {
    let chain = AnvilChain::start();
    let (near, _mid, _far, relay) = through_a_charging_connector(&chain);
    // A packet the next hop rejected is not paid for (connector#1446), so it leaves no
    // value behind: the next run's first packet is short of the price again.
    let first = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "1000", "--yes"],
    );
    assert_eq!(first.json()["outcome"], "rejected", "{}", first.stdout);
    let before = remaining(&near);

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2000", "--yes"],
    );

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["response"]["code"], "F03", "{report}");
    assert_eq!(report["credited"], 0, "{report}");
    assert_eq!(report["paid"], 0, "{report}");
    assert_eq!(remaining(&near), before);
}

#[test]
fn a_rejected_subscribe_packet_is_in_paid_and_in_the_limit_by_what_the_watermark_moved() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    // The far connector has no route for the subscribe address: it rejects the packet.
    let far = node_on(&chain);
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE);
    relay.set_connector(&far.url());
    peer_and_route(&near, &far);
    let watermark = || {
        let list = near.toon(&["channel", "list", "--json"]).json();
        list["channels"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|channel| channel["direction"] == "outbound")
            .map(|channel| {
                channel["watermark"]
                    .to_string()
                    .trim_matches('"')
                    .parse::<u64>()
                    .unwrap()
            })
            .sum::<u64>()
    };
    let remaining = || -> u64 {
        near.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let (watermark_before, remaining_before) = (watermark(), remaining());

    let run = subscribe(
        &near,
        &relay,
        &["--filter", FILTER, "--amount", "2000", "--yes"],
    );

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    let moved = watermark() - watermark_before;
    assert_eq!(report["paid"], moved, "{report}");
    assert_eq!(remaining_before - remaining(), moved);
}
