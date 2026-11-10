//! `toon zk enable` and `toon zk disable`: a TOON app's connector as a zk edge (ADR 0016).

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Duration as Span, Utc};
use connector_domain::Prepare;
use connector_zk_note::note::policy::TIERS;
use serde_json::{json, Value};
use support::fake_chain::{self, FakeChain};
use support::{Foreground, Machine, Run};

/// A Mina daemon and archive node on loopback, answering GraphQL by operation name: the slot
/// anchor a zk edge reads before it serves, and nothing else.
struct MinaStandIn {
    addr: SocketAddr,
}

impl MinaStandIn {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stand-in");
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                thread::spawn(move || answer(stream));
            }
        });
        Self { addr }
    }

    fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }
}

fn answer(stream: std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut length = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let reply = match request["operationName"].as_str() {
            Some("SlotAnchor") => json!({ "data": { "bestChain": [{ "protocolState": {
                "consensusState": { "slotSinceGenesis": "1114" },
                "blockchainState": { "utcDate": Utc::now().timestamp_millis().to_string() },
            } }] } }),
            _ => json!({ "errors": [{ "message": "not recorded" }] }),
        }
        .to_string();
        let mut writer = &stream;
        let written = write!(
            writer,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
            reply.len()
        );
        if written.is_err() {
            return;
        }
    }
}

/// A real Mina address, made from a throwaway secret: the vault.
fn vault() -> String {
    connector_zk_note::Keypair::try_from(connector_zk_note::SecKey::new(
        connector_zk_note::Scalar::from(7u64),
    ))
    .expect("a keypair")
    .public
    .into_address()
}

/// `toon zk enable` with settings the connector accepts, and `extra` after them.
fn enable(machine: &Machine, mina: &MinaStandIn, extra: &[&str]) -> Run {
    let tier = TIERS[0].to_string();
    let cap = (10 * TIERS[0]).to_string();
    let vault = vault();
    let url = mina.url();
    let mut args = vec![
        "zk",
        "enable",
        "--json",
        "--vault",
        &vault,
        "--network",
        "mina:devnet",
        "--graphql-url",
        &url,
        "--archive-url",
        &url,
        "--open-budget-cap",
        &cap,
    ];
    if !extra.contains(&"--tier") {
        args.extend_from_slice(&["--tier", &tier]);
    }
    args.extend_from_slice(extra);
    machine.toon_with(&args, |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    })
}

fn error_code(run: &Run) -> String {
    run.json()["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no error: {}", run.stdout))
        .to_owned()
}

fn config(machine: &Machine) -> String {
    std::fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors")
            .join("0")
            .join("connector.toml"),
    )
    .expect("the connector's config")
}

fn state(machine: &Machine) -> Vec<u8> {
    std::fs::read(machine.agent_node_home().join("state.json")).expect("the state")
}

/// Where `toon status` says the connector listens now: a restart may move it.
fn connector(machine: &Machine) -> SocketAddr {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"][0]["connector"]["address"]
        .as_str()
        .and_then(|address| address.parse().ok())
        .unwrap_or_else(|| panic!("no connector address in {status}"))
}

/// The connector's greeting: its answer to a greeting packet, which names how it is paid.
fn greeting(address: SocketAddr) -> Value {
    let prepare = Prepare {
        amount: 0,
        expires_at: Utc::now() + Span::minutes(5),
        greeting: true,
        destination: "g.toon.anywhere".into(),
        data: Vec::new(),
    };
    let response = reqwest::blocking::Client::new()
        .post(format!("http://{address}/ilp"))
        .header("content-type", "application/octet-stream")
        .body(prepare.encode())
        .send()
        .expect("post the greeting packet");
    assert_eq!(response.status(), 402, "a greeting is answered with 402");
    response.json().expect("a greeting")
}

fn zk_entry(greeting: &Value) -> Option<Value> {
    greeting["accepts"]
        .as_array()?
        .iter()
        .find(|entry| entry["scheme"] == "zk-note")
        .cloned()
}

fn running(machine: &Machine) -> Foreground {
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    up
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_connector_made_a_zk_edge_through_toon_greets_with_a_zk_note_entry() {
    let chain = FakeChain::start();
    let mina = MinaStandIn::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let _up = running(&machine);

    let run = enable(&machine, &mina, &["--rate", "0.40", "--yes"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let report = run.json();
    assert_eq!(report["enabled"], true, "{report}");
    assert_eq!(report["restarted"], true, "{report}");
    assert!(
        report["beside"].as_str().unwrap().contains("challenger"),
        "{report}"
    );
    let state_key = report["state_key"].as_str().unwrap().to_owned();
    let mut entry = None;
    wait_until("the connector greets as a zk edge", || {
        entry = zk_entry(&greeting(connector(&machine)));
        entry.is_some()
    });
    let entry = entry.unwrap();
    assert_eq!(entry["payTo"], vault(), "{entry}");
    assert_eq!(entry["network"], "mina:devnet", "{entry}");
    assert_eq!(entry["extra"]["stateKey"], state_key.as_str(), "{entry}");
    assert_eq!(
        entry["extra"]["verificationKeyHash"],
        connector_zk_note_mina::carried_key_hash().unwrap().as_str(),
        "{entry}"
    );

    // The rate out of MINA is rendered into the fake chain's token, at its 6 decimals:
    // 0.40 tokens per MINA is 400000 base units per 10^9 nanomina.
    let rendered = config(&machine);
    let token = format!(
        "eip155:{}/erc20:{}",
        fake_chain::CHAIN_ID,
        fake_chain::TOKEN
    );
    assert!(
        rendered.contains(&format!("to = \"{token}\"")),
        "{rendered}"
    );
    assert!(
        rendered.contains("rate = { numerator = 1, denominator = 2500 }"),
        "{rendered}"
    );
}

#[test]
fn a_connector_with_no_zk_settings_renders_the_config_it_did_before() {
    let chain = FakeChain::start();
    let mina = MinaStandIn::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let _up = running(&machine);
    let before = config(&machine);
    assert!(!before.contains("zk_note"), "{before}");

    let on = enable(&machine, &mina, &["--rate", "2/5", "--yes"]);
    assert_eq!(on.exit_code, 0, "{}", on.stdout);
    assert!(config(&machine).contains("[zk_note]"));
    let off = machine.toon(&["zk", "disable", "--yes", "--json"]);
    assert_eq!(off.exit_code, 0, "{}", off.stdout);
    assert_eq!(off.json()["restarted"], true);

    assert_eq!(config(&machine), before);
}

#[test]
fn a_tier_the_connector_does_not_accept_is_refused_before_any_restart() {
    let chain = FakeChain::start();
    let mina = MinaStandIn::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let _up = running(&machine);
    let address = connector(&machine);
    let (state_before, config_before) = (state(&machine), config(&machine));

    // Refused whether or not it was confirmed: nothing is asked of a setting that cannot hold.
    for confirm in [&["--yes"][..], &[]] {
        let mut extra = vec!["--tier", "12345", "--rate", "0.40"];
        extra.extend_from_slice(confirm);
        let run = enable(&machine, &mina, &extra);

        assert_eq!(error_code(&run), "zk_edge_invalid", "{}", run.stdout);
        let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains("12345"), "{message}");
        assert_eq!(state(&machine), state_before);
        assert_eq!(config(&machine), config_before);
        let key = machine.agent_node_home().join("connectors/0/zk-state.key");
        assert!(!key.exists(), "a refused change leaves no state key");
    }
    // The connector was never restarted: it answers where it did, as a connector with no zk
    // edge.
    assert_eq!(connector(&machine), address);
    assert_eq!(zk_entry(&greeting(address)), None);
}

#[test]
fn a_zk_edge_that_settles_on_a_chain_is_refused_without_a_rate_naming_the_pair() {
    let chain = FakeChain::start();
    let mina = MinaStandIn::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let before = state(&machine);

    let run = enable(&machine, &mina, &["--yes"]);

    assert_eq!(error_code(&run), "zk_edge_invalid", "{}", run.stdout);
    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("mina:devnet/slip44:12586"), "{message}");
    assert_eq!(state(&machine), before);
}

#[test]
fn a_malformed_rate_is_a_usage_error_and_disabling_what_is_not_on_is_refused() {
    let chain = FakeChain::start();
    let mina = MinaStandIn::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = enable(&machine, &mina, &["--rate", "-1", "--yes"]);
    assert_eq!(error_code(&run), "usage", "{}", run.stdout);

    let off = machine.toon(&["zk", "disable", "--yes", "--json"]);
    assert_eq!(error_code(&off), "zk_edge_not_enabled", "{}", off.stdout);
}
