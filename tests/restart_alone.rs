mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Duration as Span, Utc};
use connector_domain::{EnvelopeRequest, Fulfill, Prepare, Reject};
use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Where `toon status` says the connector listens now: a restart may move it.
fn connector(machine: &Machine) -> SocketAddr {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"][0]["connector"]["address"]
        .as_str()
        .and_then(|address| address.parse().ok())
        .unwrap_or_else(|| panic!("the connector has no address: {status}"))
}

/// What the fake app `name` has been asked to write, as `<path> <body in hex>` lines.
fn writes(machine: &Machine, name: &str) -> Vec<String> {
    let log: PathBuf = machine
        .agent_node_home()
        .join("apps")
        .join(name)
        .join("data")
        .join("writes.log");
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Send `body` to `destination` the way a client does: sealed to the connector's
/// identity, in a PREPARE posted to `/ilp`. `Err` is the reject's code.
fn send(address: SocketAddr, destination: &str, body: &[u8]) -> Result<Fulfill, String> {
    let identity: Value = reqwest::blocking::get(format!("http://{address}/ilp/identity"))
        .and_then(|response| response.json())
        .expect("the connector's identity");
    let public: [u8; 65] = hex::decode(
        identity["publicKey"]
            .as_str()
            .and_then(|key| key.strip_prefix("0x"))
            .expect("a public key"),
    )
    .expect("hex")
    .try_into()
    .expect("an uncompressed key");
    let envelope = EnvelopeRequest {
        method: "POST".into(),
        target: "/".into(),
        headers: vec![],
        body: body.to_vec(),
    }
    .encode();
    let (data, _) = connector_signer::giftwrap::seal_request(&envelope, &public).expect("seal");
    let prepare = Prepare {
        amount: 0,
        expires_at: Utc::now() + Span::minutes(5),
        greeting: false,
        destination: destination.into(),
        data,
    };
    let response = reqwest::blocking::Client::new()
        .post(format!("http://{address}/ilp"))
        .header("content-type", "application/octet-stream")
        .body(prepare.encode())
        .send()
        .expect("post the packet");
    assert_eq!(response.status(), 200);
    let bytes = response.bytes().expect("a body");
    Fulfill::decode(&bytes).map_err(|_| {
        Reject::decode(&bytes)
            .map(|reject| format!("{reject:?}"))
            .unwrap_or_else(|_| "not a packet".into())
    })
}

/// An agent node that is up, with its chain.
fn running() -> (Machine, FakeChain, Foreground) {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, chain, up)
}

fn with_notes(machine: &Machine) {
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "n:1",
        "--address",
        "g.toon.notes",
        "--price",
        "0",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
}

/// The connector's pid and its count of restarts, as `toon status` has them.
fn connector_run(machine: &Machine) -> (Value, Value) {
    let status = machine.toon(&["status", "--json"]).json();
    let connector = &status["agent_node"]["toon_apps"][0]["connector"];
    (connector["pid"].clone(), connector["restarts"].clone())
}

/// What `toon status` says of the app `name`.
fn app(machine: &Machine, name: &str) -> Value {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"][0]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["name"] == name)
        .unwrap_or_else(|| panic!("no app {name}: {status}"))
        .clone()
}

fn environment(machine: &Machine, name: &str) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("apps")
            .join(name)
            .join("data")
            .join("all-environment"),
    )
    .unwrap_or_default()
}

fn delivered_to_notes(machine: &Machine, body: &[u8]) {
    let before = writes(machine, "notes").len();
    send(connector(machine), "g.toon.notes", body).expect("fulfilled");
    wait_until("the app never received the packet", || {
        writes(machine, "notes").len() > before
    });
}

#[test]
fn an_app_that_comes_back_where_it_was_leaves_its_connector_alone() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = connector_run(&machine);
    let address = app(&machine, "notes")["address"].clone();

    let run = machine.toon(&["change", "notes", "--env", "A=1", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], false);
    assert_eq!(connector_run(&machine), before);
    assert_eq!(app(&machine, "notes")["address"], address);
    assert!(environment(&machine, "notes")
        .lines()
        .any(|line| line == "A=1"));
    delivered_to_notes(&machine, b"after the change");
    let text = machine.toon(&["change", "notes", "--env", "A=2", "--yes"]);
    assert!(text.stdout.contains("did not restart"), "{}", text.stdout);
}

#[test]
fn an_app_that_comes_back_somewhere_else_restarts_the_connector_once() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let (pid, _) = connector_run(&machine);
    let address = app(&machine, "notes")["address"].clone();
    // The local process runner brings the app back on other ports when this is there.
    fs::write(
        machine
            .agent_node_home()
            .join("apps")
            .join("notes")
            .join("data")
            .join(".toon-move"),
        "",
    )
    .unwrap();

    let run = machine.toon(&["change", "notes", "--env", "A=1", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], true);
    assert_ne!(connector_run(&machine).0, pid);
    assert_ne!(app(&machine, "notes")["address"], address);
    delivered_to_notes(&machine, b"at the new address");
}

#[test]
fn egress_follows_the_same_rule() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = connector_run(&machine);

    let run = machine.toon(&["change", "notes", "--allow-egress", "--yes", "--json"]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], false);
    assert_eq!(connector_run(&machine), before);

    fs::write(
        machine
            .agent_node_home()
            .join("apps")
            .join("notes")
            .join("data")
            .join(".toon-move"),
        "",
    )
    .unwrap();
    let run = machine.toon(&["change", "notes", "--no-allow-egress", "--yes", "--json"]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], true);
    delivered_to_notes(&machine, b"after egress");
}

#[test]
fn the_relay_comes_back_on_its_ports_and_its_connector_is_left_alone() {
    let (machine, _chain, _up) = running();
    let before = connector_run(&machine);
    let relay = app(&machine, "relay");

    let run = machine.toon(&["change", "relay", "--memory", "1g", "--yes", "--json"]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], false);

    let run = machine.toon(&["relay", "config", "--name", "Renamed", "--yes", "--json"]);
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], false);
    assert_eq!(connector_run(&machine), before);
    let after = app(&machine, "relay");
    assert_eq!(after["address"], relay["address"]);
    assert_eq!(after["read_address"], relay["read_address"]);
    send(connector(&machine), &machine.ephemeral_prefix(), b"ok").expect("fulfilled");
}

#[test]
fn a_relay_whose_port_was_taken_starts_on_fresh_ports_and_its_connector_restarts() {
    let (machine, _chain, _up) = running();
    let (pid, _) = connector_run(&machine);
    let relay = app(&machine, "relay");
    let write: SocketAddr = relay["address"].as_str().unwrap().parse().unwrap();
    // The relay holds the port itself until it stops; the claim another supervisor would
    // put on it is what can be taken meanwhile.
    let _taken = std::net::UdpSocket::bind(write).unwrap();

    let run = machine.toon(&["relay", "config", "--name", "Renamed", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], true);
    assert_ne!(connector_run(&machine).0, pid);
    assert_ne!(app(&machine, "relay")["address"], relay["address"]);
    send(connector(&machine), &machine.ephemeral_prefix(), b"ok").expect("fulfilled");
}

#[test]
fn a_packet_to_another_app_is_delivered_while_one_restarts() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = connector_run(&machine);

    thread::scope(|scope| {
        let changing = scope.spawn(|| {
            machine.toon(&[
                "change",
                "notes",
                "--env",
                "FAKE_APP_START_DELAY_MS=4000",
                "--yes",
                "--json",
            ])
        });
        // The old app is stopped at once, and the new one takes four seconds to start.
        thread::sleep(Duration::from_millis(1500));
        send(connector(&machine), &machine.ephemeral_prefix(), b"other").expect("fulfilled");
        // The connector answers for an app it cannot reach; the app is not handed the packet.
        let received = writes(&machine, "notes").len();
        let down = send(connector(&machine), "g.toon.notes", b"down").expect_err("a reject");
        assert!(down.contains("\"T01\""), "{down}");
        thread::sleep(Duration::from_millis(300));
        assert_eq!(writes(&machine, "notes").len(), received);
        let run = changing.join().unwrap();
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        assert_eq!(run.json()["restarted"], false);
    });
    assert_eq!(connector_run(&machine), before);
    delivered_to_notes(&machine, b"back");
}

#[test]
fn a_change_the_app_does_not_start_with_leaves_the_connector_alone() {
    let (machine, _chain, _up) = running();
    with_notes(&machine);
    let before = connector_run(&machine);
    let address = app(&machine, "notes")["address"].clone();

    let run = machine.toon(&[
        "change",
        "notes",
        "--env",
        "FAKE_APP_FAILS=1",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "app_failed");
    assert_eq!(connector_run(&machine), before);
    assert_eq!(app(&machine, "notes")["address"], address);
    delivered_to_notes(&machine, b"as it was");
}

#[test]
fn add_and_remove_still_restart_the_connector() {
    let (machine, _chain, _up) = running();
    let (pid, _) = connector_run(&machine);
    with_notes(&machine);
    let (added, _) = connector_run(&machine);
    assert_ne!(added, pid);

    let run = machine.toon(&["remove", "notes", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["restarted"], true);
    assert_ne!(connector_run(&machine).0, added);
}

#[test]
fn relay_config_without_yes_says_what_is_certain_and_what_is_possible() {
    let (machine, _chain, _up) = running();

    let before = machine.toon(&["relay", "config", "--json"]).stdout;

    let run = machine.toon(&["relay", "config", "--name", "X", "--json"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(machine.toon(&["relay", "config", "--json"]).stdout, before);
    assert_eq!(run.json()["error"]["code"], "confirmation_required");
    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("packets to it are rejected until it is back"),
        "{message}"
    );
    assert!(
        message.contains("only if relay comes back somewhere else"),
        "{message}"
    );
}
