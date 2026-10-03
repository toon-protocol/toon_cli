mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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

/// The address `notes` has when `add` is given none.
fn notes_address(machine: &Machine) -> String {
    format!("g.toon.{}.notes", machine.segment("relay"))
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

#[test]
fn add_warns_that_the_connector_restarts_and_refuses_without_yes() {
    let (machine, _chain, _up) = running();
    let before = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    let pid = machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["connector"]
        ["pid"]
        .clone();

    let run = machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "confirmation_required");
    assert!(run.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("restarts the connector"));
    assert_eq!(run.exit_code, 1);
    assert_eq!(
        fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap(),
        before
    );
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["connector"]["pid"],
        pid
    );
}

#[test]
fn remove_and_route_price_refuse_without_yes() {
    let (machine, _chain, _up) = running();

    let remove = machine.toon(&["remove", "relay", "--json"]);
    let price = machine.toon(&["route", "price", &machine.relay_prefix(), "5", "--json"]);

    assert_eq!(remove.json()["error"]["code"], "confirmation_required");
    assert_eq!(price.json()["error"]["code"], "confirmation_required");
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(status["agent_node"]["toon_apps"][0]["apps"][0]["price"], 1);
}

#[test]
fn a_packet_to_an_added_apps_address_is_delivered_to_it_after_the_restart() {
    let (machine, _chain, _up) = running();

    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "notes:1",
        "--address",
        "g.toon.notes",
        "--price",
        "0",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["added"], "notes");
    assert_eq!(run.json()["restarted"], true);
    let after = connector(&machine);
    send(after, "g.toon.notes", b"hello notes").expect("fulfilled");
    wait_until("the app never received the packet", || {
        !writes(&machine, "notes").is_empty()
    });
    let received = writes(&machine, "notes");
    let (_, body) = received[0].split_once(' ').unwrap();
    assert_eq!(hex::decode(body).unwrap(), b"hello notes");
    // The relay is still behind the connector.
    send(after, &machine.ephemeral_prefix(), b"still here").expect("fulfilled");
    wait_until("the relay never received the packet", || {
        !writes(&machine, "relay").is_empty()
    });
}

#[test]
fn status_lists_every_app_of_a_toon_app() {
    let (machine, _chain, _up) = running();
    machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--price", "3", "--yes",
    ]);

    let run = machine.toon(&["status", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let apps = run.json()["agent_node"]["toon_apps"][0]["apps"].clone();
    assert_eq!(apps[0]["name"], "relay");
    assert_eq!(apps[1]["name"], "notes");
    assert_eq!(apps[1]["running"], true);
    assert_eq!(apps[1]["prefix"], notes_address(&machine).as_str());
    assert_eq!(apps[1]["price"], 3);
    let text = machine.toon(&["status"]).stdout;
    assert!(
        text.contains("App notes of relay: running on 127.0.0.1:"),
        "{text}"
    );
    assert!(
        text.contains("App relay of relay: running on 127.0.0.1:"),
        "{text}"
    );
}

#[test]
fn an_app_at_a_url_is_delivered_to_without_running_anything() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/inbox", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            log.lock()
                .unwrap()
                .push(request.lines().next().unwrap_or("").to_owned());
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let (machine, _chain, _up) = running();

    let run = machine.toon(&[
        "add",
        "inbox",
        "--to",
        "relay",
        "--url",
        &url,
        "--address",
        "g.toon.inbox",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    send(connector(&machine), "g.toon.inbox", b"hi").expect("fulfilled");
    wait_until("the URL never received the packet", || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("POST /inbox"))
    });
    assert!(writes(&machine, "inbox").is_empty());
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["apps"][1]["url"],
        url.as_str()
    );
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["apps"][1]["running"],
        Value::Null
    );
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 0);
}

#[test]
fn remove_takes_the_app_and_its_route_away() {
    let (machine, _chain, _up) = running();
    machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--yes",
    ]);
    send(connector(&machine), &notes_address(&machine), b"one").expect("fulfilled");
    let notes = machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["apps"][1]
        ["address"]
        .as_str()
        .unwrap()
        .parse::<SocketAddr>()
        .unwrap();

    let run = machine.toon(&["remove", "notes", "--yes", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["removed"], "notes");
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"][0]["apps"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(send(connector(&machine), &notes_address(&machine), b"two").is_err());
    wait_until("the removed app is still listening", || {
        std::net::TcpStream::connect(notes).is_err()
    });
    let routes = machine.toon(&["route", "list", "--json"]).json();
    assert!(routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|route| route["prefix"] != notes_address(&machine).as_str()));
}

#[test]
fn route_price_sets_the_price_of_an_apps_route() {
    let (machine, _chain, _up) = running();

    let run = machine.toon(&[
        "route",
        "price",
        &machine.relay_prefix(),
        "7",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let routes = machine.toon(&["route", "list", "--json"]).json();
    let relay = routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["prefix"] == machine.relay_prefix().as_str())
        .expect("the relay's route");
    assert_eq!(relay["price"], 7);
}

#[test]
fn a_node_that_is_not_running_records_the_change_without_a_restart() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let run = machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "n:1", "--yes", "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["restarted"], false);
    let state: Value = serde_json::from_str(
        &fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["toon_apps"][0]["apps"][1]["name"], "notes");
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    send(connector(&machine), &notes_address(&machine), b"x").expect("fulfilled");
}

#[test]
fn a_node_that_is_not_running_refuses_a_change_the_connector_would_refuse() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let before = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();

    // A route the connector's own validation refuses.
    let run = machine.toon(&[
        "add",
        "x",
        "--to",
        "relay",
        "--url",
        "not a url",
        "--address",
        "bad",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(
        fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap(),
        before
    );
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    send(connector(&machine), &machine.ephemeral_prefix(), b"ok").expect("fulfilled");
}

#[test]
fn add_refuses_names_and_addresses_that_are_taken_or_unknown() {
    let (machine, _chain, _up) = running();

    let relay = machine.relay_prefix();
    let cases: [(&[&str], &str); 3] = [
        (
            &["add", "relay", "--to", "relay", "--image", "x", "--yes"],
            "name_taken",
        ),
        (
            &["add", "x", "--to", "nobody", "--image", "x", "--yes"],
            "unknown_name",
        ),
        (
            &[
                "add",
                "x",
                "--to",
                "relay",
                "--image",
                "x",
                "--address",
                &relay,
                "--yes",
            ],
            "route_failed",
        ),
    ];
    for (args, code) in cases {
        let mut all = args.to_vec();
        all.push("--json");
        assert_eq!(machine.toon(&all).json()["error"]["code"], code, "{args:?}");
    }
    assert_eq!(
        machine.toon(&["remove", "ghost", "--yes", "--json"]).json()["error"]["code"],
        "unknown_name"
    );
    assert_eq!(
        machine
            .toon(&["route", "price", "g.nowhere", "1", "--yes", "--json"])
            .json()["error"]["code"],
        "route_failed"
    );
    assert_eq!(
        machine
            .toon(&["add", "x", "--to", "relay", "--json"])
            .exit_code,
        2
    );
}

#[test]
fn a_failed_restart_puts_the_state_back_and_leaves_the_connector_running() {
    let (machine, _chain, _up) = running();
    let before = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    let pid = |machine: &Machine| {
        machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["connector"]["pid"]
            .clone()
    };
    let first = pid(&machine);

    // A route the connector's own validation refuses.
    let run = machine.toon(&[
        "add",
        "x",
        "--to",
        "relay",
        "--url",
        "not a url",
        "--address",
        "bad",
        "--yes",
        "--json",
    ]);

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(
        fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap(),
        before
    );
    // The config was refused before the connector stopped, so it never did.
    assert_eq!(pid(&machine), first);
    send(connector(&machine), &machine.ephemeral_prefix(), b"ok").expect("fulfilled");
}

#[test]
fn an_app_added_with_no_address_is_reached_under_its_connectors_segment() {
    let (machine, _chain, _up) = running();

    let run = machine.toon(&[
        "add", "notes", "--to", "relay", "--image", "notes:1", "--yes", "--json",
    ]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let address = format!("g.toon.{}.notes", machine.segment("relay"));
    assert_eq!(run.json()["address"], address.as_str());
    send(connector(&machine), &address, b"hello").expect("fulfilled");
    wait_until("the app never received the packet", || {
        !writes(&machine, "notes").is_empty()
    });
    // The old default, made of the name alone, goes nowhere.
    assert!(send(connector(&machine), "g.toon.notes", b"x").is_err());
}

/// The route at `prefix` as the connector describes itself over loopback.
fn described(machine: &Machine, prefix: &str) -> Value {
    let described: Value = reqwest::blocking::get(format!("http://{}/ilp", connector(machine)))
        .and_then(|response| response.json())
        .expect("the connector's self-description");
    described["routes"]
        .as_array()
        .unwrap_or_else(|| panic!("no routes: {described}"))
        .iter()
        .find(|route| route["prefix"] == prefix)
        .unwrap_or_else(|| panic!("no route {prefix}: {described}"))
        .clone()
}

#[test]
fn a_request_is_published_on_the_route_and_survives_restarts() {
    let (machine, _chain, up) = running();
    let request = serde_json::json!({
        "protocol": "nip90", "kinds": [5096, 5098], "params": { "chain": ["evm:84532"] },
        "note": "a\u{7f}b",
    });
    let file = machine.write_agent_node_file("request.json", request.to_string());
    let file = file.to_str().unwrap();

    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "notes:1",
        "--request",
        file,
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let run = machine.toon(&[
        "add",
        "plain",
        "--to",
        "relay",
        "--url",
        "http://127.0.0.1:9",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);

    let published = described(&machine, &notes_address(&machine));
    assert_eq!(published["request"], request);
    let plain = described(
        &machine,
        &format!("g.toon.{}.plain", machine.segment("relay")),
    );
    assert!(plain.get("request").is_none(), "{plain}");

    let routes = machine.toon(&["route", "list", "--json"]).json();
    let listed = routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|route| route["prefix"] == notes_address(&machine).as_str())
        .expect("the route");
    assert_eq!(listed["request"], request);

    // A restart caused by another command.
    let run = machine.toon(&[
        "route",
        "price",
        &notes_address(&machine),
        "3",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(
        described(&machine, &notes_address(&machine))["request"],
        request
    );

    let mut up = up;
    let down = machine.toon(&["down", "--json"]);
    assert_eq!(down.exit_code, 0, "{}", down.stdout);
    assert_eq!(up.exit_code(), 0);
    let again = machine.start(&["up", "--foreground", "--json"]);
    again.report();
    assert_eq!(
        described(&machine, &notes_address(&machine))["request"],
        request
    );
}

#[test]
fn a_request_that_is_not_an_object_is_refused_and_nothing_changes() {
    let (machine, _chain, _up) = running();
    let before = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    let pid = machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"][0]["connector"]
        ["pid"]
        .clone();
    for (name, text) in [
        ("array.json", "[1]"),
        ("null.json", r#"{"a": null}"#),
        ("nested.json", r#"{"a": [{"b": null}]}"#),
        ("text.json", "not json"),
        ("scalar.json", "3"),
    ] {
        let file = machine.write_agent_node_file(name, text);
        let run = machine.toon(&[
            "add",
            "notes",
            "--to",
            "relay",
            "--image",
            "notes:1",
            "--request",
            file.to_str().unwrap(),
            "--yes",
            "--json",
        ]);
        assert_eq!(run.exit_code, 2, "{name}: {}", run.stdout);
        assert_eq!(run.json()["error"]["code"], "usage", "{name}");
    }
    let run = machine.toon(&[
        "add",
        "notes",
        "--to",
        "relay",
        "--image",
        "notes:1",
        "--request",
        "/no/such/file",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 2, "{}", run.stdout);
    assert_eq!(
        fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap(),
        before
    );
    let after = machine.toon(&["status", "--json"]).json();
    assert_eq!(after["agent_node"]["toon_apps"][0]["connector"]["pid"], pid);
}
