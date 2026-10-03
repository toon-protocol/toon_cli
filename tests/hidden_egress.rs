//! A hidden agent node's commands send every request they make through the overlay's
//! proxy, by name (ADR 0003). The tests reach a fake relay or connector at a `.anyone`
//! name that only the loopback stand-in's proxy knows, declared with `TOON_OVERLAY_NAMES`:
//! a request that was resolved on this machine could not reach it.

mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

use serde_json::{json, Value};
use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::fake_faucet::FakeFaucet;
use support::fake_remote_relay::FakeRemoteRelay;
use support::{Foreground, Machine, Run};

/// What the peering's channel is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;
const PRICE: u64 = 1000;
const BROADCAST_PRICE: u64 = 10;
const SUBSCRIBE: &str = "g.toon.subscribe";
const FILTER: &str = r#"{"kinds":[1]}"#;

/// `name` at `port`, declared to stand for `address`: one entry of `TOON_OVERLAY_NAMES`.
fn declare(name: &str, port: u16, address: SocketAddr) -> String {
    format!("{name}:{port}={address}")
}

fn toon(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    })
}

fn toon_declaring(machine: &Machine, names: &str, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command
            .env("TOON_PASSPHRASE", support::PASSPHRASE)
            .env("TOON_OVERLAY_NAMES", names);
    })
}

fn error_of(run: &Run) -> Value {
    run.json()["error"].clone()
}

fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..300 {
        if condition() {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("what was waited for did not happen");
}

struct Far {
    machine: Machine,
    _up: Foreground,
}

impl Far {
    fn url(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["connector"]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the connector has no address: {status}"));
        format!("http://{address}/ilp")
    }
}

/// A clearnet agent node, running, to be peered with.
fn far_on(chain: &AnvilChain) -> Far {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let evm = toon(&machine, &["wallet", "show", "--json"]).json()["wallet"]["chains"]["evm"][0]
        ["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Far { machine, _up: up }
}

/// A hidden agent node on `chain`, running, whose proxy knows `names`.
fn hidden_near(chain: &AnvilChain, names: &str) -> Far {
    let machine = Machine::new();
    let init = machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--evm-token",
        &chain.token(),
        "--evm-decimals",
        &support::anvil_chain::TOKEN_DECIMALS.to_string(),
        "--allow-plaintext-peers",
    ]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(init.json()["toon_apps"][0]["reach"], "hidden");
    let evm = toon(&machine, &["wallet", "show", "--json"]).json()["wallet"]["chains"]["evm"][0]
        ["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start_with(&["up", "--foreground", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        command.env("TOON_OVERLAY_NAMES", names);
    });
    up.report();
    Far { machine, _up: up }
}

fn peer_and_route(near: &Far, far: &Far, address: &str) {
    let peered = toon(
        &near.machine,
        &[
            "peer",
            "add",
            &far.url(),
            "--deposit",
            &DEPOSIT.to_string(),
            "--yes",
            "--id",
            "far",
        ],
    );
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
    let routed = toon(&near.machine, &["route", "add", address, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
}

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

/// A relay that sells its feed, behind the clearnet node `far`.
fn selling(far: &Far) -> FakeRemoteRelay {
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE);
    let status = far.machine.toon(&["status", "--json"]).json();
    let app = status["agent_node"]["toon_apps"][0]["name"]
        .as_str()
        .expect("a TOON app")
        .to_owned();
    let added = toon(
        &far.machine,
        &[
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
        ],
    );
    assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    relay.set_connector(&far.url());
    relay
}

fn relay_address(relay: &FakeRemoteRelay) -> SocketAddr {
    relay.url().trim_start_matches("ws://").parse().unwrap()
}

#[test]
fn a_hidden_agent_node_subscribes_reads_and_follows_a_relay_at_an_anyone_name() {
    let chain = AnvilChain::start();
    let far = far_on(&chain);
    let relay = selling(&far);
    relay.also_at("relay.anyone:7100");
    let names = declare("relay.anyone", 7100, relay_address(&relay));
    let near = hidden_near(&chain, &names);
    peer_and_route(&near, &far, SUBSCRIBE);
    let url = "ws://relay.anyone:7100";

    // The information document and the balance are read through the proxy: the name is not
    // one this machine can resolve.
    let subscribed = toon(
        &near.machine,
        &[
            "relay",
            "subscribe",
            url,
            "--filter",
            FILTER,
            "--amount",
            "1000",
            "--yes",
            "--json",
        ],
    );
    assert_eq!(
        subscribed.exit_code, 0,
        "{}{}",
        subscribed.stdout, subscribed.stderr
    );
    assert_eq!(subscribed.json()["outcome"], "subscribed");
    let listed = toon(&near.machine, &["relay", "subscriptions", "--json"]);
    assert_eq!(listed.json()["subscriptions"][0]["current"], true);
    assert_eq!(listed.json()["subscriptions"][0]["balance"], 1000);

    relay.broadcast(event(1));
    let query = toon(
        &near.machine,
        &["event", "query", url, "--filter", FILTER, "--json"],
    );
    assert_eq!(query.exit_code, 0, "{}{}", query.stdout, query.stderr);
    assert_eq!(query.json()["events"], json!([event(1)]));

    // The supervisor's feed is one; the one `follow` opens is the other.
    eventually(|| relay.open_feeds() == 1);
    let follow = near.machine.start(&["event", "follow", url, "--json"]);
    eventually(|| relay.open_feeds() == 2);
    relay.broadcast(event(2));
    // The feed starts with what the relay stores, as the draft says.
    let first: Value = serde_json::from_str(&follow.line()).expect("one document");
    let second: Value = serde_json::from_str(&follow.line()).expect("one document");
    assert_eq!((first, second), (event(1), event(2)));
}

/// A server that answers every request with a NIP-11 document that names `connector`, and
/// its sealing key, as where a write is paid for.
fn information_document(connector: &str, address: &str) -> SocketAddr {
    let body = json!({
        "name": "far",
        "toon": {
            "ilp_address": address,
            "connector_url": connector,
            "connector_seal_key": support::seal_key(connector),
            "price": 1,
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
    address
}

#[test]
fn a_hidden_agent_node_publishes_to_a_relay_at_an_anyone_name() {
    let chain = AnvilChain::start();
    let far = far_on(&chain);
    let document = information_document(&far.url(), &far.machine.relay_prefix());
    let names = declare("write.anyone", 7100, document);
    let near = hidden_near(&chain, &names);
    peer_and_route(&near, &far, &far.machine.relay_prefix());

    let published = toon(
        &near.machine,
        &[
            "event",
            "publish",
            "--relay",
            "ws://write.anyone:7100",
            "--kind",
            "1",
            "--content",
            "across",
            "--yes",
            "--json",
        ],
    );

    assert_eq!(
        published.exit_code, 0,
        "{}{}",
        published.stdout, published.stderr
    );
    assert_eq!(published.json()["outcome"], "published");
}

#[test]
fn a_command_reaches_an_onion_endpoint_the_supervisor_published() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let endpoint = init.json()["toon_apps"][0]["onion_endpoint"]
        .as_str()
        .expect("an onion endpoint")
        .to_owned();
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    // No name was declared: this is an endpoint of the supervisor's own stand-in, which the
    // command finds at `overlay/loopback`.
    let relay = machine.relay_prefix();
    let sent = machine.toon(&[
        "send",
        &relay,
        "--amount",
        "0",
        "--yes",
        "--seal-to",
        &format!("http://{endpoint}/ilp"),
        "--json",
    ]);

    assert_eq!(sent.exit_code, 0, "{}{}", sent.stdout, sent.stderr);
    assert_eq!(sent.json()["outcome"], "fulfilled");
    assert!(machine.agent_node_home().join("overlay/loopback").exists());
}

#[test]
fn a_request_to_a_host_that_is_neither_local_nor_published_gets_the_proxys_refusal() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&[
        "event",
        "query",
        "ws://nowhere.invalid:7100",
        "--filter",
        "{}",
        "--json",
    ]);

    let error = error_of(&run);
    assert_eq!(error["code"], "query_failed", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("The overlay's proxy"), "{message}");
    assert!(!message.contains("lookup"), "{message}");
}

#[test]
fn a_seal_to_on_a_clearnet_name_goes_through_the_proxy_too() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    // The connector's own `send` would dial a clearnet name directly and resolve it here.
    let relay = machine.relay_prefix();
    let sent = machine.toon(&[
        "send",
        &relay,
        "--amount",
        "0",
        "--yes",
        "--seal-to",
        "http://nowhere.invalid:7100/ilp",
        "--json",
    ]);

    let error = error_of(&sent);
    assert_eq!(error["code"], "send_failed", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("nowhere.invalid"), "{message}");
    assert!(!message.contains("dns error"), "{message}");
    assert!(!message.contains("lookup"), "{message}");
}

#[test]
fn without_an_overlay_a_hidden_agent_node_dials_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let relay = FakeRemoteRelay::start(SUBSCRIBE, PRICE, BROADCAST_PRICE);

    for args in [
        vec![
            "event",
            "query",
            "ws://relay.example:7100",
            "--filter",
            "{}",
        ],
        vec![
            "event",
            "publish",
            "--relay",
            "ws://relay.example:7100",
            "--kind",
            "1",
            "--yes",
        ],
        vec![
            "relay",
            "subscribe",
            "ws://relay.example:7100",
            "--filter",
            FILTER,
            "--amount",
            "1000",
            "--yes",
        ],
    ] {
        let mut args = args;
        args.push("--json");
        let run = machine.toon_with(&args, |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_OVERLAY", "none");
        });
        assert_eq!(
            error_of(&run)["code"],
            "overlay_unavailable",
            "{args:?}: {}",
            run.stdout
        );
    }
    // A relay on this machine is not dialled through an overlay, and is not refused for the
    // lack of one: a plain local endpoint has nothing to hide.
    let local = machine.toon_with(
        &["event", "query", &relay.url(), "--filter", "{}", "--json"],
        |command| {
            command.env("TOON_OVERLAY", "none");
        },
    );
    assert_eq!(local.exit_code, 0, "{}{}", local.stdout, local.stderr);
}

#[test]
fn a_hidden_agent_node_asks_the_chain_and_the_faucet_through_the_proxy() {
    let chain = FakeChain::start_unfunded();
    let faucet = FakeFaucet::funding(chain.funded());
    let rpc: SocketAddr = chain
        .rpc_url()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .parse()
        .unwrap();
    let faucet_address: SocketAddr = faucet.url().trim_start_matches("http://").parse().unwrap();
    let names = [
        declare("rpc.anyone", 8545, rpc),
        declare("faucet.anyone", 8080, faucet_address),
    ]
    .join(",");
    let machine = Machine::new();
    let init = toon_declaring(
        &machine,
        &names,
        &[
            "init",
            "--json",
            "--accept-anyone-terms",
            "--evm-rpc-url",
            "http://rpc.anyone:8545",
            "--faucet-url",
            "http://faucet.anyone:8080",
        ],
    );
    assert_eq!(init.exit_code, 0, "{}{}", init.stdout, init.stderr);

    let fund = toon_declaring(&machine, &names, &["wallet", "fund", "--json"]);

    assert_eq!(fund.exit_code, 0, "{}{}", fund.stdout, fund.stderr);
    assert_eq!(faucet.asked().len(), 1);
    assert_eq!(fund.json()["lacking"].as_array().map(Vec::len), Some(0));
    let balances = toon_declaring(&machine, &names, &["wallet", "balances", "--json"]);
    assert_eq!(
        balances.exit_code, 0,
        "{}{}",
        balances.stdout, balances.stderr
    );
    assert!(chain.count("eth_getBalance") > 0);
}

#[test]
fn a_hidden_agent_node_with_no_overlay_does_not_ask_a_remote_chain() {
    let machine = Machine::new();
    let init = machine.init_with(&["--evm-rpc-url", "https://rpc.example/evm"]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let run = machine.toon_with(&["wallet", "balances", "--json"], |command| {
        command
            .env("TOON_PASSPHRASE", support::PASSPHRASE)
            .env("TOON_OVERLAY", "none");
    });

    assert_eq!(
        error_of(&run)["code"],
        "overlay_unavailable",
        "{}",
        run.stdout
    );
}

#[test]
fn a_hidden_agent_node_reads_a_wss_relay_at_an_anyone_name_through_the_proxy() {
    let chain = AnvilChain::start();
    let far = far_on(&chain);
    let relay = selling(&far);
    let relay = relay.with_tls(&["relay.anyone"]);
    relay.also_at("relay.anyone:7100");
    // The name is one only the proxy knows, and it takes the connection to the TLS front.
    let names = declare("relay.anyone", 7100, relay.wss_address());
    let near = hidden_near(&chain, &names);
    peer_and_route(&near, &far, SUBSCRIBE);
    let url = "wss://relay.anyone:7100";
    let root = relay.root_file();
    let trusting = |args: &[&str]| {
        near.machine.toon_with(args, |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_OVERLAY_NAMES", &names)
                .env("TOON_TRUSTED_ROOT", &root);
        })
    };

    // The information document is read over `https://` and the balance with it, both
    // through the proxy: this machine cannot resolve the name.
    let subscribed = trusting(&[
        "relay",
        "subscribe",
        url,
        "--filter",
        FILTER,
        "--amount",
        "1000",
        "--yes",
        "--json",
    ]);
    assert_eq!(
        subscribed.exit_code, 0,
        "{}{}",
        subscribed.stdout, subscribed.stderr
    );
    assert_eq!(subscribed.json()["outcome"], "subscribed");

    relay.broadcast(event(1));
    let query = trusting(&["event", "query", url, "--filter", FILTER, "--json"]);
    assert_eq!(query.exit_code, 0, "{}{}", query.stdout, query.stderr);
    assert_eq!(query.json()["events"], json!([event(1)]));
}

#[test]
fn a_hidden_agent_node_holds_a_subscription_at_a_relay_on_this_machine_directly() {
    let chain = AnvilChain::start();
    let far = far_on(&chain);
    let relay = selling(&far);
    // The stand-in's proxy refuses loopback, as the real daemon does, so a feed that is
    // dialled through it reaches nothing.
    let near = hidden_near(&chain, "");
    peer_and_route(&near, &far, SUBSCRIBE);
    let url = relay.url();
    assert!(url.starts_with("ws://127.0.0.1:"), "{url}");

    let subscribed = toon(
        &near.machine,
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
    );
    assert_eq!(
        subscribed.exit_code, 0,
        "{}{}",
        subscribed.stdout, subscribed.stderr
    );
    eventually(|| relay.open_feeds() == 1);
    relay.broadcast(event(1));

    let status = toon(&near.machine, &["status", "--json"]).json();
    let own = format!(
        "ws://{}",
        status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no address: {status}"))
    );
    eventually(|| {
        let query = toon(
            &near.machine,
            &["event", "query", &own, "--filter", FILTER, "--json"],
        );
        query.json()["events"] == json!([event(1)])
    });
}
