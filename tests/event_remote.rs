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
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("would send 1 base"));
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
fn a_publish_that_fails_before_a_packet_is_sent_is_not_counted() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    // The relay pins a key that reads as one and is no point on the curve, so the packet
    // cannot be sealed.
    let relay = document(json!({
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": far.url(),
        "connector_seal_key": support::UNSEALABLE_KEY,
        "price": PRICE,
    }));
    peer_and_route(&near, &far);
    let remaining =
        || near.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"].clone();
    let before = remaining();

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "send_failed", "{}", run.stdout);
    assert_eq!(run.exit_code, 1);
    assert_eq!(remaining(), before);
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
    assert!(
        error["message"].as_str().unwrap().contains(&format!(
            "toon peer add {} --deposit <amount> --yes`",
            far.url()
        )),
        "{error}"
    );
    assert_eq!(run.exit_code, 1);
    let peers = near.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"].as_array().map(Vec::len), Some(0));
}

#[test]
fn yes_without_relay_is_refused() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "publish", "--kind", "1", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
    assert_eq!(run.exit_code, 2);
}

#[test]
fn an_amount_below_the_relays_price_is_a_usage_error_and_sends_nothing() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = document(json!({
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": far.url(),
        "connector_seal_key": support::seal_key(&far.url()),
        "price": 5,
    }));
    peer_and_route(&near, &far);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--amount", "4", "--yes", "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
    assert_eq!(run.exit_code, 2);
    assert_eq!(events_at(&near, &far), 0);
}

/// What `mid` charges to forward a write to `far`'s relay.
const FORWARD: u64 = 101;

/// `near` peered with `mid`, `mid` peered with `far`, and `mid` charging `FORWARD` for the
/// route to `far`'s relay. Returns the relay's information document's URL.
fn through_a_charging_connector(chain: &AnvilChain) -> (Node, Node, Node, String) {
    let near = node_on(chain);
    let mid = node_on(chain);
    let far = node_on(chain);
    let relay = information_document(&far);
    let prefix = far.machine.relay_prefix();
    let peered = |from: &Node, to: &Node, id: &str| {
        let run = from.toon(&[
            "peer",
            "add",
            &to.url(),
            "--deposit",
            &DEPOSIT.to_string(),
            "--yes",
            "--id",
            id,
        ]);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    };
    peered(&near, &mid, "mid");
    peered(&mid, &far, "far");
    let routed = mid.toon(&[
        "route",
        "add",
        &prefix,
        "--peer",
        "far",
        "--price",
        &FORWARD.to_string(),
    ]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    let routed = near.toon(&["route", "add", &prefix, "--peer", "mid"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    (near, mid, far, relay)
}

fn remaining(near: &Node) -> String {
    near.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .as_str()
        .expect("remaining_today")
        .to_owned()
}

#[test]
fn a_stated_amount_pays_a_connector_that_charges_to_forward_and_without_it_is_rejected() {
    let chain = AnvilChain::start();
    let (near, _mid, far, relay) = through_a_charging_connector(&chain);
    let before: u128 = remaining(&near).parse().expect("a number");
    let watermark = outbound_watermark(&near);

    let rejected = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);
    assert_eq!(rejected.exit_code, 1, "{}", rejected.stdout);
    let report = rejected.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], "F03", "{report}");
    // The report states the path's cost, which is the amount the write then pays.
    assert_eq!(report["cost"], FORWARD.to_string(), "{report}");
    assert_eq!(report["complete"], true, "{report}");
    let text = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes",
    ]);
    assert_eq!(text.exit_code, 1, "{}", text.stdout);
    assert!(
        text.stdout.contains(&format!("--amount {FORWARD}")),
        "{}",
        text.stdout
    );
    assert_eq!(events_at(&near, &far), 0);

    let amount = FORWARD.to_string();
    let published = near.toon(&[
        "event",
        "publish",
        "--relay",
        &relay,
        "--kind",
        "1",
        "--content",
        "via",
        "--amount",
        &amount,
        "--yes",
        "--json",
    ]);
    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    assert_eq!(report["paid"], FORWARD);
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
        Value::Array(vec![report["event"].clone()]),
        "{}",
        query.stdout
    );
    // The two rejected writes were never carried, so the connector does not pay for them:
    // the voucher of the write that was carried is signed from what `mid` reports, and
    // the day's count, read after each packet, can still hold what a rejected write's
    // voucher showed before the next forward dropped it (#93, connector#1446).
    let after: u128 = remaining(&near).parse().expect("a number");
    let moved = outbound_watermark(&near) - watermark;
    assert_eq!(moved, u128::from(FORWARD), "{moved}");
    assert!(before - after >= moved, "{} < {moved}", before - after);
}

#[test]
fn without_yes_the_refusal_states_the_amount_and_nothing_is_paid() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let before = remaining(&near);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--amount", "101", "--json",
    ]);

    let error = run.json()["error"].clone();
    assert_eq!(error["code"], "not_confirmed", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("101 base"), "{message}");
    assert!(message.contains("price is 1"), "{message}");
    assert_eq!(run.exit_code, 1);
    assert_eq!(remaining(&near), before);
    assert_eq!(events_at(&near, &far), 0);
}

#[test]
fn an_amount_over_the_spending_limit_is_refused_and_sends_nothing() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let set = near.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "50",
        "--max-per-day",
        "1000",
        "--json",
    ]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--amount", "51", "--yes", "--json",
    ]);

    assert_eq!(
        run.json()["error"]["code"],
        "spending_limit",
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
    assert_eq!(events_at(&near, &far), 0);
}

#[test]
fn a_write_is_sealed_to_the_published_key_and_never_dials_the_connector_url() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let hint = support::spy::start();
    let relay = document(json!({
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": hint.url(),
        "connector_seal_key": support::seal_key(&far.url()).trim_start_matches("0x"),
        "price": PRICE,
    }));
    peer_and_route(&near, &far);

    let published = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);

    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    assert_eq!(report["paid"], PRICE);
    assert_eq!(hint.connections(), 0);
    assert_eq!(events_at(&near, &far), 1);
}

#[test]
fn a_toon_object_without_a_whole_write_edge_is_not_payable() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    peer_and_route(&near, &far);
    let whole = json!({
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": far.url(),
        "connector_seal_key": support::seal_key(&far.url()),
        "price": PRICE,
    });
    let mut cases = Vec::new();
    for field in [
        "ilp_address",
        "connector_url",
        "connector_seal_key",
        "price",
    ] {
        let mut toon = whole.clone();
        toon.as_object_mut().unwrap().remove(field);
        cases.push((toon, field.to_owned()));
    }
    // 65 bytes, but not an uncompressed key.
    let compressed = format!("05{}", "ab".repeat(64));
    for bad in ["", "04ab", "zz", &"04".repeat(66), &compressed] {
        let mut toon = whole.clone();
        toon["connector_seal_key"] = json!(bad);
        cases.push((toon, "connector_seal_key".to_owned()));
    }
    for (toon, missing) in cases {
        let relay = document(toon.clone());
        let run = near.toon(&[
            "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
        ]);
        let error = run.json()["error"].clone();
        assert_eq!(error["code"], "relay_not_payable", "{toon}: {}", run.stdout);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains(&format!("has no `{missing}`"))
                || error["message"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("has a `{missing}` that is not")),
            "{toon}: {error}"
        );
        assert_eq!(run.exit_code, 1);
    }
    assert_eq!(events_at(&near, &far), 0);
}

/// How many kind 1 events the relay behind `far` holds.
fn events_at(near: &Node, far: &Node) -> usize {
    let query = near.machine.toon(&[
        "event",
        "query",
        &far.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
        "--json",
    ]);
    query.json()["events"].as_array().map(Vec::len).unwrap_or(0)
}

/// What the watermarks of `node`'s outbound channels add up to, as `toon channel list` shows.
fn outbound_watermark(node: &Node) -> u128 {
    let list = node.toon(&["channel", "list", "--json"]).json();
    list["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .filter(|channel| channel["direction"] == "outbound")
        .map(|channel| match &channel["watermark"] {
            Value::String(text) => text.parse::<u128>().expect("a watermark"),
            other => other.as_u64().expect("a watermark") as u128,
        })
        .sum()
}

fn remaining_today(node: &Node) -> u128 {
    node.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .as_str()
        .expect("remaining_today")
        .parse()
        .expect("a number")
}

/// A destination `far` has no route for, reached through `near`'s peering.
const NOWHERE: &str = "g.toon.nowhere";

fn route_nowhere(near: &Node) {
    let routed = near.toon(&["route", "add", NOWHERE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
}

/// Run `send` to `NOWHERE` and check that what it reports as paid is what the outbound
/// watermark moved by, and what the day's limit lost.
fn send_to_nowhere_is_counted_as_the_watermark_moved(near: &Node, code: &str) {
    let watermark = outbound_watermark(near);
    let remaining = remaining_today(near);

    let run = near.toon(&["send", NOWHERE, "--amount", "7", "--yes", "--json"]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], code, "{report}");
    let moved = outbound_watermark(near) - watermark;
    assert_eq!(report["paid"], moved as u64, "{report}");
    assert_eq!(remaining - remaining_today(near), moved, "{report}");
}

#[test]
fn a_packet_rejected_by_the_far_connector_is_reported_and_counted_as_the_watermark_moved() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    peer_and_route(&near, &far);
    route_nowhere(&near);

    send_to_nowhere_is_counted_as_the_watermark_moved(&near, "F02");
}

#[test]
fn a_packet_rejected_because_the_far_connector_is_not_running_is_counted_as_the_watermark_moved() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    peer_and_route(&near, &far);
    route_nowhere(&near);
    let down = far.machine.toon(&["down", "--json"]);
    assert_eq!(down.exit_code, 0, "{}{}", down.stdout, down.stderr);

    send_to_nowhere_is_counted_as_the_watermark_moved(&near, "T01");
}

#[test]
fn a_packet_rejected_by_the_agent_nodes_own_connector_is_not_counted() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    peer_and_route(&near, &far);
    let watermark = outbound_watermark(&near);
    let remaining = remaining_today(&near);

    let run = near.toon(&["send", "g.nobody.here", "--amount", "7", "--yes", "--json"]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(run.json()["paid"], 0);
    assert_eq!(outbound_watermark(&near), watermark);
    assert_eq!(remaining_today(&near), remaining);
}

#[test]
fn a_fulfilled_packet_reports_and_counts_its_amount() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    peer_and_route(&near, &far);
    let remaining = remaining_today(&near);

    let run = near.toon(&[
        "send",
        &far.machine.relay_prefix(),
        "--amount",
        "1",
        "--seal-to",
        &far.url(),
        "--yes",
        "--json",
    ]);

    assert_eq!(run.json()["outcome"], "fulfilled", "{}", run.stdout);
    assert_eq!(run.json()["paid"], 1);
    assert_eq!(remaining - remaining_today(&near), 1);
}

#[test]
fn a_publish_rejected_by_the_far_connector_reports_and_counts_the_watermark_moved() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = document(json!({
        "ilp_address": NOWHERE,
        "connector_url": far.url(),
        "connector_seal_key": support::seal_key(&far.url()),
        "price": 7,
    }));
    peer_and_route(&near, &far);
    route_nowhere(&near);
    let watermark = outbound_watermark(&near);
    let remaining = remaining_today(&near);

    let run = near.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    let moved = outbound_watermark(&near) - watermark;
    assert_eq!(report["paid"], moved as u64, "{report}");
    assert_eq!(remaining - remaining_today(&near), moved);
}

/// Stops the connectors `toon up` started, and lets them run again when dropped, so that a
/// test that fails does not leave one stopped.
struct Stopped(u32);

impl Stopped {
    fn connectors_of(up: &Foreground) -> Self {
        let stopped = Self(up.pid());
        stopped.signal("STOP");
        stopped
    }

    fn signal(&self, name: &str) {
        let _ = std::process::Command::new("pkill")
            .args([&format!("-{name}"), "-P", &self.0.to_string()])
            .status();
    }
}

impl Drop for Stopped {
    fn drop(&mut self) {
        self.signal("CONT");
    }
}

#[test]
fn a_publish_the_connector_does_not_answer_reports_its_cost_and_its_event() {
    let chain = AnvilChain::start();
    let near = node_on(&chain);
    let far = node_on(&chain);
    let relay = information_document(&far);
    peer_and_route(&near, &far);
    let watermark = outbound_watermark(&near);
    let remaining = remaining_today(&near);
    // The far connector takes the packet and never answers; the packet's expiry is far off
    // and the command line gives up first, which a wait longer than the expiry never does.
    let _stopped = Stopped::connectors_of(&far._up);

    let run = near.machine.toon_with(
        &[
            "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
        ],
        |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_PACKET_WAIT_MS", "4000");
        },
    );

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let report = run.json();
    assert_eq!(report["error"]["code"], "send_failed", "{report}");
    let message = report["error"]["message"].as_str().expect("a message");
    assert!(message.contains("has expired"), "{message}");
    assert!(message.contains("run again"), "{message}");
    let id = report["event"]["id"].as_str().expect("the event's id");
    assert!(message.contains(id), "{message}");
    let moved = outbound_watermark(&near) - watermark;
    assert_eq!(report["paid"], moved as u64, "{report}");
    // The cost sentence is there when the packet cost something, and only then.
    assert_eq!(message.contains("It cost"), moved > 0, "{message}");
    assert_eq!(remaining - remaining_today(&near), moved, "{report}");

    let text = near.machine.toon_with(
        &[
            "event", "publish", "--relay", &relay, "--kind", "1", "--yes",
        ],
        |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_PACKET_WAIT_MS", "4000");
        },
    );
    assert_eq!(text.exit_code, 1, "{}", text.stderr);
    assert!(text.stderr.contains("has expired"), "{}", text.stderr);
    assert!(
        text.stderr.contains("`toon event query`"),
        "{}",
        text.stderr
    );
}
