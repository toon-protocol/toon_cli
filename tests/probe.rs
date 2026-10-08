//! `toon probe`: a packet sent to learn what a path costs, which by default carries nothing.

mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::fake_remote_relay::FakeRemoteRelay;
use support::{Foreground, Machine, Run};

const DEPOSIT: u128 = 1_000_000;
/// What the fake relay's subscribe route charges, and what `mid` charges to forward to it.
const PRICE: u64 = 1000;
const FORWARD: u64 = 1100;
const SUBSCRIBE: &str = "g.toon.subscribe";
/// What the priced app of the single-node tests charges.
const APP_PRICE: u64 = 500;

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

    fn url(&self) -> String {
        for _ in 0..100 {
            let status = self.machine.toon(&["status", "--json"]).json();
            if let Some(address) =
                status["agent_node"]["toon_apps"][0]["connector"]["address"].as_str()
            {
                return format!("http://{address}/ilp");
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("the connector has no address");
    }

    fn remaining(&self) -> u128 {
        self.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
            .as_str()
            .expect("remaining_today")
            .parse()
            .expect("a number")
    }

    /// What the watermarks of the outbound channels add up to, as `toon channel list` shows.
    fn outbound_watermark(&self) -> u128 {
        let list = self.toon(&["channel", "list", "--json"]).json();
        list["channels"]
            .as_array()
            .expect("channels")
            .iter()
            .filter(|channel| channel["direction"] == "outbound")
            .map(|channel| match &channel["watermark"] {
                serde_json::Value::String(text) => text.parse::<u128>().expect("a watermark"),
                other => other.as_u64().expect("a watermark") as u128,
            })
            .sum()
    }
}

/// An app that records each request it receives, head and body, and answers 201.
fn app() -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/inbox", listener.local_addr().expect("address"));
    let (tx, seen) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).into_owned();
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let wanted = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    body.len() >= wanted
                });
                if read == 0 || complete {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
            let _ = stream.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\nConnection: close\r\n\r\nmade!",
            );
        }
    });
    (url, seen)
}

/// An agent node on a fake chain with an app at `url` sold for `price`.
fn with_app(
    url: &str,
    price: u64,
    configure: impl Fn(&mut std::process::Command),
) -> (Node, FakeChain) {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let up = machine.start_with(&["up", "--foreground", "--json"], |command| {
        configure(command)
    });
    up.report();
    let node = Node { machine, _up: up };
    let added = node.toon(&[
        "add",
        "inbox",
        "--to",
        "relay",
        "--url",
        url,
        "--address",
        "g.toon.inbox",
        "--price",
        &price.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    (node, chain)
}

fn priced() -> (Node, FakeChain, std::sync::mpsc::Receiver<String>) {
    let (url, seen) = app();
    let (node, chain) = with_app(&url, APP_PRICE, |_| {});
    (node, chain, seen)
}

#[test]
fn a_probe_of_a_route_its_direct_peer_terminates_states_the_price_and_pays_what_it_carried() {
    let chain = AnvilChain::start();
    let (near, far, relay) = beside_a_priced_route(&chain);
    let before = near.remaining();
    let sealed = far.url();

    // One base unit, so that the probe pays for something.
    let run = near.toon(&[
        "probe",
        SUBSCRIBE,
        "--seal-to",
        &sealed,
        "--amount",
        "1",
        "--yes",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["cost"], PRICE.to_string(), "{report}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["paid"], 1);
    assert_eq!(report["amount"], 1);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.stderr, "");
    assert_eq!(near.remaining(), before - 1);
    assert_eq!(relay.posts(), 0);
    let text = near.toon(&[
        "probe",
        SUBSCRIBE,
        "--seal-to",
        &sealed,
        "--amount",
        "1",
        "--yes",
    ]);
    assert_eq!(text.exit_code, 0);
    assert!(
        text.stdout
            .contains(&format!("`toon send --amount {PRICE}`")),
        "{}",
        text.stdout
    );
}

#[test]
fn a_probe_with_no_amount_of_a_route_its_direct_peer_terminates_states_the_price_and_pays_nothing()
{
    let chain = AnvilChain::start();
    let (near, far, relay) = beside_a_priced_route(&chain);
    let before = near.remaining();
    let watermark = near.outbound_watermark();
    let sealed = far.url();

    let run = near.toon(&["probe", SUBSCRIBE, "--seal-to", &sealed, "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["cost"], PRICE.to_string(), "{report}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["paid"], 0, "{report}");
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.stderr, "");
    assert_eq!(near.remaining(), before);
    assert_eq!(near.outbound_watermark(), watermark);
    assert_eq!(relay.posts(), 0);
    let text = near.toon(&["probe", SUBSCRIBE, "--seal-to", &sealed]);
    assert_eq!(text.exit_code, 0);
    assert!(
        text.stdout
            .contains(&format!("`toon send --amount {PRICE}`")),
        "{}",
        text.stdout
    );
}

#[test]
fn a_probe_carries_the_request_it_is_given() {
    let (node, _chain, seen) = priced();
    let body = node.machine.home().join("body.json");
    std::fs::write(&body, br#"{"text":"hello"}"#).unwrap();

    // Paid for, so that the packet reaches the app and shows what it carried.
    let run = node.toon(&[
        "probe",
        "g.toon.inbox",
        "--amount",
        &APP_PRICE.to_string(),
        "--method",
        "PUT",
        "--path",
        "/some/path?x=1",
        "--body",
        body.to_str().unwrap(),
        "--yes",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["status"], 201);
    assert_eq!(report["response"]["body"], "made!");
    assert_eq!(report["paid"], APP_PRICE);
    assert_eq!(run.exit_code, 0);
    let request = seen
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the app received the request")
        .to_ascii_lowercase();
    assert!(
        request.starts_with("put /inbox/some/path?x=1 "),
        "{request}"
    );
    assert!(request.ends_with(r#"{"text":"hello"}"#), "{request}");
}

#[test]
fn a_probe_of_a_route_that_costs_nothing_states_a_cost_of_0_and_delivers_the_request() {
    let (url, seen) = app();
    let (node, _chain) = with_app(&url, 0, |_| {});

    let run = node.toon(&["probe", "g.toon.inbox", "--json"]);

    // A route that charges nothing is fulfilled: the app receives the request.
    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["paid"], 0);
    assert_eq!(run.exit_code, 0);
    seen.recv_timeout(std::time::Duration::from_secs(5))
        .expect("the app received the request");
}

#[test]
fn a_probe_to_an_address_with_no_route_exits_1_with_no_cost() {
    let (node, _chain, _seen) = priced();

    let run = node.toon(&["probe", "g.nobody.here", "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], "F02");
    assert!(report.get("cost").is_none() && report.get("complete").is_none());
    assert_eq!(report["paid"], 0);
    assert_eq!(run.exit_code, 1);
    let text = node.toon(&["probe", "g.nobody.here"]);
    assert!(
        text.stdout.starts_with("Rejected with F02"),
        "{}",
        text.stdout
    );
    assert!(!text.stdout.contains("costs"), "{}", text.stdout);
}

#[test]
fn a_rejected_send_with_no_answer_to_the_cost_carries_none() {
    let (node, _chain, _seen) = priced();

    let run = node.toon(&["send", "g.nobody.here", "--amount", "0", "--yes", "--json"]);

    let report = run.json();
    assert_eq!(report["reject"]["code"], "F02");
    assert!(report.get("cost").is_none() && report.get("complete").is_none());
}

#[test]
fn a_probe_with_an_amount_and_no_yes_sends_nothing() {
    let (node, _chain, seen) = priced();
    let before = node.remaining();

    let run = node.toon(&["probe", "g.toon.inbox", "--amount", "5", "--json"]);

    assert_eq!(
        run.json()["error"]["code"],
        "not_confirmed",
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
    assert_eq!(node.remaining(), before);
    assert!(seen.try_recv().is_err());
}

#[test]
fn a_probe_amount_past_the_per_command_limit_is_refused() {
    let (node, _chain, _seen) = priced();
    let set = node.toon(&[
        "limit",
        "set",
        "--max-per-command",
        "4",
        "--max-per-day",
        "100000",
        "--json",
    ]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let run = node.toon(&["probe", "g.toon.inbox", "--amount", "5", "--yes", "--json"]);

    assert_eq!(
        run.json()["error"]["code"],
        "spending_limit",
        "{}",
        run.stdout
    );
}

#[test]
fn a_probe_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["probe", "g.toon.relay", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn a_probe_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["probe", "g.toon.relay", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn a_probe_from_a_hidden_agent_node_sealed_to_a_clearnet_name_forms_its_own_request() {
    let chain = AnvilChain::start();
    // The name stands for a port that forwards to the far connector once it listens. It is
    // not an onion endpoint, so the connector's `send` would dial it directly: `toon` forms
    // the request itself and sends it through the overlay's proxy.
    let forwarder = TcpListener::bind("127.0.0.1:0").expect("bind");
    let names = format!(
        "far.example:7100={}",
        forwarder.local_addr().expect("address")
    );
    let near = node_with(&chain, Some(&names));
    let (far, relay) = far_with_relay(&chain);
    let connector: SocketAddr = far.url()["http://".len()..far.url().len() - "/ilp".len()]
        .parse()
        .expect("a socket address");
    forward(forwarder, connector);
    peer_with(&near, &far, "far");
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);

    let run = near.machine.toon_with(
        &[
            "probe",
            SUBSCRIBE,
            "--seal-to",
            "http://far.example:7100/ilp",
            "--amount",
            "1",
            "--yes",
            "--method",
            "PUT",
            "--json",
        ],
        |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_OVERLAY_NAMES", &names);
        },
    );

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}{}", run.stderr);
    assert_eq!(report["cost"], PRICE.to_string(), "{report}");
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(run.exit_code, 0);
    assert_eq!(relay.posts(), 0);
}

/// Carry every connection `listener` accepts to `target`, both ways.
fn forward(listener: TcpListener, target: SocketAddr) {
    thread::spawn(move || {
        for inbound in listener.incoming().flatten() {
            let Ok(outbound) = std::net::TcpStream::connect(target) else {
                continue;
            };
            for (mut from, mut to) in [
                (inbound.try_clone().unwrap(), outbound.try_clone().unwrap()),
                (outbound, inbound),
            ] {
                thread::spawn(move || {
                    let _ = std::io::copy(&mut from, &mut to);
                    let _ = to.shutdown(std::net::Shutdown::Write);
                });
            }
        }
    });
}

fn node_on(chain: &AnvilChain) -> Node {
    node_with(chain, None)
}

/// A clearnet agent node, or, given `names` for its overlay's proxy, a hidden one.
fn node_with(chain: &AnvilChain, names: Option<&str>) -> Node {
    let machine = Machine::new();
    let init = match names {
        None => machine.init_on_anvil(chain, true),
        Some(_) => machine.init_with(&[
            "--evm-rpc-url",
            &chain.rpc_url(),
            "--evm-token",
            &chain.token(),
            "--evm-decimals",
            &support::anvil_chain::TOKEN_DECIMALS.to_string(),
            "--allow-plaintext-peers",
        ]),
    };
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    if names.is_some() {
        assert_eq!(init.json()["toon_apps"][0]["reach"], "hidden");
    }
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start_with(&["up", "--foreground", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        if let Some(names) = names {
            command.env("TOON_OVERLAY_NAMES", names);
        }
    });
    up.report();
    Node { machine, _up: up }
}

fn peer_with(from: &Node, to: &Node, id: &str) {
    peer_with_fee(from, to, id, 0);
}

fn peer_with_fee(from: &Node, to: &Node, id: &str, fee: u64) {
    let peered = from.toon(&[
        "peer",
        "add",
        &to.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--fee",
        &fee.to_string(),
        "--yes",
        "--id",
        id,
    ]);
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
}

/// A relay `far` sells its subscribe route for `PRICE`, behind a node of its own.
fn far_with_relay(chain: &AnvilChain) -> (Node, FakeRemoteRelay) {
    let far = node_on(chain);
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, 10);
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

/// `near` peered with `far` directly, which terminates the subscribe route for `PRICE`.
fn beside_a_priced_route(chain: &AnvilChain) -> (Node, Node, FakeRemoteRelay) {
    let near = node_on(chain);
    let (far, relay) = far_with_relay(chain);
    peer_with(&near, &far, "far");
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    (near, far, relay)
}

/// `near` peered with `mid`, `mid` peered with `far` and keeping `FORWARD` of each packet it
/// forwards, which `near` reaches through `mid`.
fn through_a_charging_connector(chain: &AnvilChain) -> (Node, Node, Node, FakeRemoteRelay) {
    let near = node_on(chain);
    let mid = node_on(chain);
    let (far, relay) = far_with_relay(chain);
    peer_with(&near, &mid, "mid");
    peer_with_fee(&mid, &far, "far", FORWARD);
    let routed = mid.toon(&["route", "add", SUBSCRIBE, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    let routed = near.toon(&["route", "add", SUBSCRIBE, "--peer", "mid"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    (near, mid, far, relay)
}

#[test]
fn an_amount_0_probe_through_a_connector_that_charges_to_forward_gives_a_partial_cost() {
    let chain = AnvilChain::start();
    let (near, _mid, far, _relay) = through_a_charging_connector(&chain);
    let before = near.remaining();

    let sealed = far.url();
    let run = near.toon(&["probe", SUBSCRIBE, "--seal-to", &sealed, "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], "R01", "{report}");
    assert_eq!(report["cost"], FORWARD.to_string(), "{report}");
    assert_eq!(report["complete"], false, "{report}");
    assert_eq!(report["paid"], 0);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(near.remaining(), before);
    let text = near.toon(&["probe", SUBSCRIBE, "--seal-to", &sealed]);
    assert!(
        text.stdout.contains("not the whole cost"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains(&format!("`--amount {FORWARD} --yes`")),
        "{}",
        text.stdout
    );
}

#[test]
fn a_rejected_probe_with_an_amount_counts_what_the_channels_moved_by() {
    let chain = AnvilChain::start();
    let (near, _mid, far, _relay) = through_a_charging_connector(&chain);
    let before = near.remaining();
    let amount = u128::from(FORWARD) + 1;

    // The amount pays the connector in between and leaves one unit for the route past it.
    let sealed = far.url();
    let run = near.toon(&[
        "probe",
        SUBSCRIBE,
        "--seal-to",
        &sealed,
        "--amount",
        &amount.to_string(),
        "--yes",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let paid = u128::from(report["paid"].as_u64().expect("paid"));
    assert!(paid > 0 && paid <= amount, "{report}");
    assert_eq!(near.remaining(), before - paid);
}

#[test]
fn a_probe_whose_amount_covers_the_path_is_delivered_and_paid_for() {
    let chain = AnvilChain::start();
    let (near, _mid, far, relay) = through_a_charging_connector(&chain);
    let before = near.remaining();
    let amount = u128::from(FORWARD + PRICE);

    let sealed = far.url();
    let run = near.toon(&[
        "probe",
        SUBSCRIBE,
        "--seal-to",
        &sealed,
        "--amount",
        &amount.to_string(),
        "--yes",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert!(report["response"]["status"].is_number(), "{report}");
    assert_eq!(report["paid"], amount as u64);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(near.remaining(), before - amount);
    assert_eq!(relay.posts(), 1);
}
