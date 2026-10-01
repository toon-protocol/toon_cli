mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Duration as Span, Utc};
use connector_domain::{EnvelopeRequest, Fulfill, Prepare};
use serde_json::Value;
use support::fake_chain::FakeChain;
use support::Machine;

fn relay_dir(machine: &Machine) -> PathBuf {
    machine.agent_node_home().join("apps").join("relay")
}

fn connector(report: &Value) -> SocketAddr {
    report["connector"]["address"]
        .as_str()
        .and_then(|address| address.parse().ok())
        .unwrap_or_else(|| panic!("the report has no connector address: {report}"))
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// What the fake relay has been asked to write, as `<path> <body in hex>` lines.
fn writes(machine: &Machine) -> Vec<String> {
    fs::read_to_string(relay_dir(machine).join("data").join("writes.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Send `body` to `destination` through the connector at `address`, the way a client does:
/// sealed to the connector's identity, in a PREPARE posted to `/ilp`.
fn send(address: SocketAddr, destination: &str, body: &[u8]) -> Fulfill {
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
    Fulfill::decode(&response.bytes().expect("a body")).expect("the connector fulfilled it")
}

#[test]
fn a_packet_to_the_relays_address_is_delivered_to_the_relay() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--json"]);
    let address = connector(&up.report());

    send(address, "g.toon.relay.ephemeral", b"hello relay");

    wait_until("the relay never received the packet", || {
        !writes(&machine).is_empty()
    });
    let received = writes(&machine);
    assert_eq!(received.len(), 1, "{received:?}");
    let (path, body) = received[0].split_once(' ').unwrap();
    assert_eq!(path, "/write-ephemeral");
    assert_eq!(hex::decode(body).unwrap(), b"hello relay");
}

#[test]
fn the_relay_is_started_with_the_wallets_relay_identity_key() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--json"]);
    up.report();

    let key = fs::read(relay_dir(&machine).join("identity.key")).unwrap();
    assert_eq!(key.len(), 32);
    assert_eq!(
        fs::read_to_string(relay_dir(&machine).join("data").join("environment")).unwrap(),
        format!("NOSTR_SECRET_KEY={}\n", hex::encode(&key))
    );
    // Not the connector's identity key, which settles nothing and signs nothing for the relay.
    let connector_key =
        fs::read(machine.agent_node_home().join("connectors/0/identity.key")).unwrap();
    assert_ne!(key, connector_key);
}

#[test]
fn the_connectors_config_routes_the_relays_write_and_its_free_ephemeral_write() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--json"]);
    up.report();

    let status = machine.toon(&["status", "--json"]).json();
    let relay = &status["agent_node"]["toon_apps"][0]["apps"][0];
    let write = relay["address"].as_str().expect("the relay's address");
    let config = fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/connector.toml"),
    )
    .unwrap();
    assert!(
        config.contains(&format!(
            "prefix = \"g.toon.relay\"\nhandler_url = \"http://{write}/write\"\nprice = 1\n"
        )),
        "{config}"
    );
    assert!(config.contains(&format!(
        "prefix = \"g.toon.relay.ephemeral\"\nhandler_url = \"http://{write}/write-ephemeral\"\nprice = 0\n"
    )), "{config}");
    // The write port is on loopback, so only this machine's connector reaches it.
    assert!(write.starts_with("127.0.0.1:"), "{write}");
}

#[test]
fn status_shows_the_relay_as_an_app_of_the_first_toon_app() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let up = machine.start(&["up", "--json"]);
    up.report();

    let run = machine.toon(&["status", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let app = &run.json()["agent_node"]["toon_apps"][0];
    assert_eq!(app["name"], "relay");
    assert_eq!(app["apps"][0]["name"], "relay");
    assert_eq!(app["apps"][0]["running"], true);
    let text = machine.toon(&["status"]);
    assert!(
        text.stdout
            .contains("App relay of relay: running on 127.0.0.1:"),
        "{}",
        text.stdout
    );
}

#[test]
fn down_stops_the_relay() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let mut up = machine.start(&["up", "--json"]);
    up.report();
    let relay: SocketAddr = machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"]
        [0]["apps"][0]["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    assert_eq!(machine.toon(&["down"]).exit_code, 0);

    assert_eq!(up.exit_code(), 0);
    wait_until("the relay is still listening", || {
        std::net::TcpStream::connect(relay).is_err()
    });
}

#[test]
fn up_fails_with_app_failed_when_the_relay_does_not_start() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let run = machine.toon_with(&["up", "--json"], |command| {
        command.env("TOON_APP_COMMAND", "/nonexistent/relay");
    });

    assert_eq!(run.json()["error"]["code"], "app_failed");
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn up_fails_when_the_relay_stops() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let mut up = machine.start(&["up", "--json"]);
    let report = up.report();
    let pid = report["connector"]["pid"].as_u64().unwrap();
    let relay: SocketAddr = machine.toon(&["status", "--json"]).json()["agent_node"]["toon_apps"]
        [0]["apps"][0]["address"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let relay_pid = relay_pid(relay);

    std::process::Command::new("kill")
        .arg(relay_pid.to_string())
        .status()
        .unwrap();

    assert_eq!(up.exit_code(), 1);
    wait_until("the connector outlived the failed supervisor", || {
        !std::path::Path::new("/proc").join(pid.to_string()).exists()
    });
}

/// The process listening on `address`, found through `/proc`.
fn relay_pid(address: SocketAddr) -> u32 {
    let inode = fs::read_to_string("/proc/net/tcp")
        .unwrap()
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let port = u16::from_str_radix(fields[1].split_once(':')?.1, 16).ok()?;
            (port == address.port() && fields[3] == "0A").then(|| fields[9].to_owned())
        })
        .next()
        .expect("a listening socket");
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::read_link(fd.path())
                .is_ok_and(|link| link.to_string_lossy() == format!("socket:[{inode}]"))
            {
                return pid;
            }
        }
    }
    panic!("no process listens on {address}");
}
