mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run};

/// What the peering's channel is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

/// The price of a write the followed relay states.
const PRICE: u64 = 1;

/// The asset code the tipped agent node settles in, and its scale.
const CODE: &str = "TST";
const SCALE: &str = "6";

struct Node {
    machine: Machine,
    up: Foreground,
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

    /// The agent identity's public key; also keeps the secret for the supervisor.
    fn identity(&self) -> String {
        let signed = self.toon(&["event", "sign", "--kind", "1", "--json"]);
        signed.json()["event"]["pubkey"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(
            &std::fs::read(self.machine.agent_node_home().join("state.json")).unwrap(),
        )
        .unwrap()
    }

    /// The loopback port the supervisor serves the tip route on.
    fn tip_port(&self) -> u64 {
        self.state()["toon_apps"][0]["tip"]["port"]
            .as_u64()
            .unwrap_or_else(|| panic!("no tip route in {}", self.state()))
    }

    fn routes(&self) -> Vec<Value> {
        let description: Value = reqwest::blocking::get(self.url()).unwrap().json().unwrap();
        description["routes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn tip_route(&self) -> Option<Value> {
        let prefix = format!("g.toon.{}.tip", self.machine.segment("relay"));
        self.routes()
            .into_iter()
            .find(|route| route["prefix"] == prefix.as_str())
    }

    fn events(&self, kinds: &[u64]) -> Vec<Value> {
        self.machine.stored_events(kinds)
    }

    /// Deliver a tip to the tip route's app the way the connector does.
    fn deliver(&self, amount: &str, chain: &str, body: &Value) -> u16 {
        reqwest::blocking::Client::new()
            .post(format!("http://127.0.0.1:{}/", self.tip_port()))
            .header("x-toon-amount", amount)
            .header("x-toon-chain", chain)
            .header("x-toon-payer", "0xpayer")
            .body(body.to_string())
            .send()
            .unwrap()
            .status()
            .as_u16()
    }

    fn tips_status(&self) -> Value {
        self.machine.toon(&["status", "--json"]).json()["agent_node"]["tips"].clone()
    }
}

/// Start an agent node on `chain` whose profile follows `relay`, with its agent identity kept.
fn node_on(chain: &AnvilChain, relay: Option<&str>) -> Node {
    let machine = Machine::new();
    let decimals = SCALE;
    let (rpc_url, token) = (chain.rpc_url(), chain.token());
    let mut args = vec![
        "--clearnet",
        "toon.example.com",
        "--evm-rpc-url",
        &rpc_url,
        "--evm-token",
        &token,
        "--evm-decimals",
        decimals,
        "--evm-asset-code",
        CODE,
        "--allow-plaintext-peers",
    ];
    if let Some(relay) = relay {
        args.extend(["--relay-url", relay]);
    }
    let init = machine.init_with(&args);
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
    let node = Node {
        machine,
        up,
        address,
    };
    node.identity();
    node
}

/// A relay's information document naming `far`'s connector as where a write is paid for,
/// served while `up` holds.
struct Relay {
    url: String,
    up: Arc<AtomicBool>,
}

fn relay_of(far: &Node) -> Relay {
    let body = json!({ "name": "far", "toon": {
        "ilp_address": far.machine.relay_prefix(),
        "connector_url": far.url(),
        "connector_seal_key": support::seal_key(&far.url()),
        "price": PRICE,
    }})
    .to_string();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    let up = Arc::new(AtomicBool::new(true));
    let answering = Arc::clone(&up);
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            if !answering.load(Ordering::SeqCst) {
                continue;
            }
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
    Relay {
        url: format!("ws://{address}"),
        up,
    }
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

/// Wait until `check` holds.
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !check() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// A node that tips can be turned on for, with a far relay it follows.
struct Pair {
    near: Node,
    far: Node,
    relay: Relay,
    _chain: AnvilChain,
}

fn pair() -> Pair {
    let chain = AnvilChain::start();
    let far = node_on(&chain, None);
    let relay = relay_of(&far);
    let near = node_on(&chain, Some(&relay.url));
    peer_and_route(&near, &far);
    Pair {
        near,
        far,
        relay,
        _chain: chain,
    }
}

/// The payer: signs tip receipts with its own agent identity.
struct Payer(Machine);

impl Payer {
    fn new() -> Self {
        let machine = Machine::new();
        let init = machine.init_with(&[]);
        assert_eq!(init.exit_code, 0, "{}", init.stdout);
        Self(machine)
    }

    fn sign(&self, kind: u64, tags: Value) -> Value {
        self.0
            .toon_with(
                &[
                    "event",
                    "sign",
                    "--kind",
                    &kind.to_string(),
                    "--tags",
                    &tags.to_string(),
                    "--json",
                ],
                |command| {
                    command.env("TOON_PASSPHRASE", support::PASSPHRASE);
                },
            )
            .json()["event"]
            .clone()
    }

    /// A tip receipt for `recipient`, stating `code` and `scale`.
    fn receipt(&self, recipient: &str, code: &str, scale: &str, extra: Vec<Value>) -> Value {
        let mut tags = vec![
            json!(["p", recipient]),
            json!(["e", "ab".repeat(32), "wss://r.example"]),
            json!(["a", format!("30023:{recipient}:note")]),
            json!(["amount", "1000", code, scale]),
        ];
        tags.extend(extra);
        self.sign(9850, Value::Array(tags))
    }
}

fn acknowledgements(node: &Node) -> Vec<Value> {
    node.events(&[9851])
}

fn tag<'a>(event: &'a Value, name: &str) -> Vec<&'a Value> {
    event["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tag| tag[0] == name)
        .collect()
}

#[test]
fn enabling_tips_serves_the_route_and_publishes_the_tip_route_to_both_relays() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();

    let enabled = near.toon(&["tip", "enable", "--minimum", "5", "--yes", "--json"]);

    assert_eq!(enabled.exit_code, 0, "{}{}", enabled.stdout, enabled.stderr);
    let route = near.tip_route().expect("GET /ilp lists the tip route");
    assert_eq!(route["payment"], true, "{route}");
    assert_eq!(route["price"].to_string().trim_matches('"'), "5", "{route}");
    for relay in [near, &pair.far] {
        let routes = relay.events(&[10850]);
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(routes[0]["pubkey"], identity.as_str());
        let named = |name: &str| tag(&routes[0], name)[0][1].as_str().unwrap().to_owned();
        assert_eq!(
            named("ilp_address"),
            format!("g.toon.{}.tip", near.machine.segment("relay"))
        );
        assert!(named("connector_url").starts_with("https://toon.example.com"));
        assert_eq!(
            named("connector_seal_key").trim_start_matches("0x").len(),
            130
        );
    }
    let status = near.tips_status();
    assert_eq!(status["enabled"], true, "{status}");
    assert_eq!(status["minimum"], 5);
    assert_eq!(status["acknowledgements"]["waiting"], 0);
    let text = near.machine.toon(&["status"]).stdout;
    assert!(text.contains("Tips: on"), "{text}");

    // Another minimum changes the price and publishes the tip route again.
    thread::sleep(Duration::from_millis(1100));
    let again = near.toon(&["tip", "enable", "--minimum", "7", "--yes", "--json"]);

    assert_eq!(again.exit_code, 0, "{}{}", again.stdout, again.stderr);
    let route = near
        .tip_route()
        .expect("GET /ilp still lists the tip route");
    assert_eq!(route["price"].to_string().trim_matches('"'), "7", "{route}");
    let id = &again.json()["published"]["id"];
    assert_ne!(id, &enabled.json()["published"]["id"]);
    for relay in [near, &pair.far] {
        let routes = relay.events(&[10850]);
        assert!(routes.iter().any(|route| &route["id"] == id), "{routes:?}");
    }
    assert_eq!(near.tips_status()["minimum"], 7);
}

#[test]
fn the_tip_route_is_still_served_after_the_agent_node_is_started_again() {
    let mut pair = pair();
    let identity = pair.near.identity();
    let payer = Payer::new();
    let enabled = pair.near.toon(&["tip", "enable", "--yes", "--json"]);
    assert_eq!(enabled.exit_code, 0, "{}{}", enabled.stdout, enabled.stderr);
    let before = payer.receipt(&identity, CODE, SCALE, vec![]);
    assert_eq!(pair.near.deliver("1000", "evm", &before), 200);
    eventually("the acknowledgement", || {
        acknowledgements(&pair.near).len() == 1
    });

    assert_eq!(pair.near.machine.toon(&["down"]).exit_code, 0);
    assert_eq!(pair.near.up.exit_code(), 0);
    pair.near.up = pair.near.machine.start(&["up", "--foreground", "--json"]);
    pair.near.up.report();

    let route = pair.near.tip_route().expect("the route is written again");
    assert_eq!(route["payment"], true);
    // The supervisor serves the route again, and remembers what it acknowledged.
    assert_eq!(pair.near.deliver("1000", "evm", &before), 200);
    let after = payer.receipt(&identity, CODE, SCALE, vec![]);
    assert_eq!(pair.near.deliver("1000", "evm", &after), 200);
    eventually("the second acknowledgement", || {
        acknowledgements(&pair.near).len() == 2
    });
    thread::sleep(Duration::from_secs(2));
    let receipts: Vec<Value> = acknowledgements(&pair.near)
        .iter()
        .map(|acknowledgement| tag(acknowledgement, "receipt")[0][1].clone())
        .collect();
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    assert!(receipts.contains(&before["id"]) && receipts.contains(&after["id"]));
}

#[test]
fn a_tip_receipt_is_acknowledged_once_to_both_relays() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();
    let payer = Payer::new();
    let enabled = near.toon(&["tip", "enable", "--yes", "--json"]);
    assert_eq!(enabled.exit_code, 0, "{}{}", enabled.stdout, enabled.stderr);
    let receipt = payer.receipt(&identity, CODE, SCALE, vec![]);

    assert_eq!(near.deliver("1020", "evm", &receipt), 200);

    eventually("the acknowledgement reaches both relays", || {
        acknowledgements(near).len() == 1 && acknowledgements(&pair.far).len() == 1
    });
    let acknowledgement = &acknowledgements(near)[0];
    assert_eq!(acknowledgement["pubkey"], identity.as_str());
    assert_eq!(acknowledgement["content"], "");
    assert_eq!(
        tag(acknowledgement, "amount")[0],
        &json!(["amount", "1020", CODE, SCALE])
    );
    assert_eq!(
        tag(acknowledgement, "receipt")[0][1],
        receipt["id"],
        "{acknowledgement}"
    );
    assert_eq!(tag(acknowledgement, "p")[0][1], receipt["pubkey"]);
    assert_eq!(tag(acknowledgement, "e"), tag(&receipt, "e"));
    assert_eq!(tag(acknowledgement, "a"), tag(&receipt, "a"));

    // The same receipt again is a tip that arrived, and is not acknowledged a second time.
    assert_eq!(near.deliver("1020", "evm", &receipt), 200);
    thread::sleep(Duration::from_secs(2));
    assert_eq!(acknowledgements(near).len(), 1);
    assert_eq!(acknowledgements(&pair.far).len(), 1);
    assert!(
        near.up.stderr().contains("acknowledged before"),
        "{}",
        near.up.stderr()
    );
}

#[test]
fn a_receipt_that_asks_to_be_anonymous_is_acknowledged_without_naming_the_payer() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();
    let payer = Payer::new();
    near.toon(&["tip", "enable", "--yes", "--json"]);
    let receipt = payer.receipt(&identity, CODE, SCALE, vec![json!(["anon"])]);

    assert_eq!(near.deliver("1000", "evm", &receipt), 200);

    eventually("the acknowledgement", || acknowledgements(near).len() == 1);
    assert!(tag(&acknowledgements(near)[0], "p").is_empty());
}

#[test]
fn a_tip_that_is_not_for_this_identity_or_not_signed_or_in_another_unit_is_not_acknowledged() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();
    let payer = Payer::new();
    near.toon(&["tip", "enable", "--yes", "--json"]);

    let elsewhere = payer.receipt(&"cd".repeat(32), CODE, SCALE, vec![]);
    let mut forged = payer.receipt(&identity, CODE, SCALE, vec![]);
    forged["content"] = json!("changed after signing");
    let other_code = payer.receipt(&identity, "EUR", SCALE, vec![]);
    let other_scale = payer.receipt(&identity, CODE, "2", vec![]);
    let two_recipients = payer.sign(
        9850,
        json!([
            ["p", identity],
            ["p", "cd".repeat(32)],
            ["amount", "1", CODE, SCALE]
        ]),
    );
    let not_a_receipt = payer.sign(1, json!([["p", identity]]));

    for (receipt, chain, reason) in [
        (&elsewhere, "evm", "not for this agent identity"),
        (&forged, "evm", "not signed"),
        (&other_code, "evm", "states EUR"),
        (&other_scale, "evm", "states TST 2"),
        (&two_recipients, "evm", "not for this agent identity"),
        (&not_a_receipt, "evm", "not a tip receipt"),
        (
            &payer.receipt(&identity, CODE, SCALE, vec![]),
            "solana",
            "no unit",
        ),
    ] {
        assert_eq!(near.deliver("1000", chain, receipt), 200);
        eventually("the reason is logged", || near.up.stderr().contains(reason));
    }
    thread::sleep(Duration::from_secs(1));
    assert!(acknowledgements(near).is_empty());
    assert!(acknowledgements(&pair.far).is_empty());
    assert_eq!(near.tips_status()["acknowledgements"]["waiting"], 0);
}

#[test]
fn an_acknowledgement_waits_while_the_followed_relay_is_down_and_is_published_when_it_is_back() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();
    let payer = Payer::new();
    near.toon(&["tip", "enable", "--yes", "--json"]);
    pair.relay.up.store(false, Ordering::SeqCst);
    let receipt = payer.receipt(&identity, CODE, SCALE, vec![]);

    assert_eq!(near.deliver("1000", "evm", &receipt), 200);

    eventually(
        "the own relay has it and the followed one is waited for",
        || {
            let tips = near.tips_status();
            acknowledgements(near).len() == 1 && tips["acknowledgements"]["waiting"] == 1
        },
    );
    let waiting = near.tips_status()["acknowledgements"].clone();
    assert!(
        waiting["last_error"]
            .as_str()
            .is_some_and(|why| !why.is_empty()),
        "{waiting}"
    );
    assert!(acknowledgements(&pair.far).is_empty());

    pair.relay.up.store(true, Ordering::SeqCst);

    eventually("the followed relay has it", || {
        acknowledgements(&pair.far).len() == 1
            && near.tips_status()["acknowledgements"]["waiting"] == 0
    });
    assert_eq!(
        acknowledgements(near).len(),
        1,
        "the own relay is not written twice"
    );
}

#[test]
fn disabling_tips_removes_the_route_and_publishes_a_deletion() {
    let pair = pair();
    let near = &pair.near;
    let identity = near.identity();
    near.toon(&["tip", "enable", "--yes", "--json"]);
    assert!(near.tip_route().is_some());

    let disabled = near.toon(&["tip", "disable", "--yes", "--json"]);

    assert_eq!(
        disabled.exit_code, 0,
        "{}{}",
        disabled.stdout, disabled.stderr
    );
    assert!(near.tip_route().is_none());
    for relay in [near, &pair.far] {
        let deletions = relay.events(&[5]);
        assert_eq!(deletions.len(), 1, "{deletions:?}");
        assert_eq!(
            tag(&deletions[0], "a")[0][1],
            format!("10850:{identity}:").as_str()
        );
        assert_eq!(tag(&deletions[0], "k")[0][1], "10850");
    }
    let status = near.tips_status();
    assert_eq!(status["enabled"], false, "{status}");
    let text = near.machine.toon(&["status"]).stdout;
    assert!(text.contains("Tips: off"), "{text}");
}

fn running_on_fake_chain() -> (Machine, Foreground) {
    let chain = Box::leak(Box::new(FakeChain::start()));
    let machine = Machine::new();
    let init = machine.init_on(chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, up)
}

fn error_code(run: &Run) -> String {
    run.json()["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no error: {}", run.stdout))
        .to_owned()
}

fn keep_identity(machine: &Machine) {
    let signed = machine.toon_with(&["event", "sign", "--kind", "1", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    assert_eq!(signed.exit_code, 0, "{}", signed.stdout);
}

#[test]
fn enabling_without_a_kept_identity_secret_is_refused_and_changes_nothing() {
    let (machine, _up) = running_on_fake_chain();
    std::fs::remove_file(machine.agent_node_home().join("agent.key")).ok();
    let before = std::fs::read(machine.agent_node_home().join("state.json")).unwrap();

    let run = machine.toon(&["tip", "enable", "--yes", "--json"]);

    assert_eq!(error_code(&run), "agent_key_not_kept");
    assert_eq!(
        std::fs::read(machine.agent_node_home().join("state.json")).unwrap(),
        before
    );
}

#[test]
fn enabling_with_no_relay_in_the_profile_is_refused() {
    let (machine, _up) = running_on_fake_chain();
    keep_identity(&machine);
    let path = machine.agent_node_home().join("state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["relay_url"] = Value::Null;
    machine.write_agent_node_file("state.json", state.to_string());

    let run = machine.toon(&["tip", "enable", "--yes", "--json"]);

    assert_eq!(error_code(&run), "no_profile_relay");
}

#[test]
fn enabling_restarts_a_connector_and_so_needs_yes() {
    let (machine, _up) = running_on_fake_chain();
    keep_identity(&machine);

    let run = machine.toon(&["tip", "enable", "--json"]);

    assert_eq!(error_code(&run), "confirmation_required");
}

#[test]
fn enabling_needs_the_agent_node_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    keep_identity(&machine);

    let run = machine.toon(&["tip", "enable", "--yes", "--json"]);

    assert_eq!(error_code(&run), "not_running");
}

#[test]
fn disabling_when_tips_are_off_says_so() {
    let (machine, _up) = running_on_fake_chain();

    let run = machine.toon(&["tip", "disable", "--yes", "--json"]);

    assert_eq!(error_code(&run), "tips_not_enabled");
}

#[test]
fn status_says_tips_are_off_by_default() {
    let (machine, _up) = running_on_fake_chain();

    let status = machine.toon(&["status", "--json"]).json();

    let tips = &status["agent_node"]["tips"];
    assert_eq!(tips["enabled"], false, "{status}");
    assert_eq!(tips["acknowledgements"]["waiting"], 0);
}
