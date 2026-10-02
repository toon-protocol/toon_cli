mod support;

use std::fs;

use serde_json::Value;
use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

/// What each of the two channels is opened with, in the token's base units: one USDC.
const DEPOSIT: u128 = 1_000_000;

fn with_passphrase(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

/// The first EVM address a message names.
fn address_in(message: &str) -> String {
    let start = message.find("0x").expect("an address in the message");
    message[start..start + 42].to_owned()
}

/// Where `toon status` says the connector of the TOON app `name` listens.
fn listening(machine: &Machine, name: &str) -> String {
    let status = machine.toon(&["status", "--json"]).json();
    status["agent_node"]["toon_apps"]
        .as_array()
        .and_then(|apps| apps.iter().find(|app| app["name"] == name))
        .and_then(|app| app["connector"]["address"].as_str())
        .unwrap_or_else(|| panic!("{name} has no connector address: {status}"))
        .to_owned()
}

/// What the fake app `name` has been asked to write.
fn writes(machine: &Machine, name: &str) -> Vec<String> {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("apps")
            .join(name)
            .join("data")
            .join("writes.log"),
    )
    .unwrap_or_default()
    .lines()
    .map(str::to_owned)
    .collect()
}

/// An agent node on a chain that moves money, running, whose first wallet address holds
/// ten deposits.
fn running(chain: &AnvilChain) -> (Machine, Foreground) {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]).json();
    let evm = shown["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address");
    chain.fund(evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, up)
}

fn create_second(machine: &Machine, extra: &[&str]) -> Run {
    let mut args = vec![
        "create",
        "second",
        "--image",
        "second:1",
        "--clearnet",
        "second.example.com",
        "--json",
    ];
    args.extend_from_slice(extra);
    with_passphrase(machine, &args)
}

#[test]
fn a_second_toon_app_is_peered_both_ways_and_a_packet_crosses_each_way() {
    let chain = AnvilChain::start();
    let (machine, _up) = running(&chain);
    let deposit = DEPOSIT.to_string();
    let peering = ["--deposit", deposit.as_str(), "--yes"];

    // The new connector's key must hold gas and the token before it starts: nothing is
    // created, and the message names the address.
    let refused = create_second(&machine, &peering);
    assert_eq!(
        refused.json()["error"]["code"],
        "unfunded",
        "{}",
        refused.stdout
    );
    let state = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    assert!(!state.contains("second"), "{state}");
    let address = address_in(refused.json()["error"]["message"].as_str().unwrap());
    chain.fund(&address, DEPOSIT * 10);

    let created = create_second(&machine, &peering);

    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    let report = created.json();
    assert_eq!(report["created"]["name"], "second");
    assert_eq!(report["created"]["connector"], 1);
    assert_eq!(report["started"], true);
    let peerings = report["peerings"].as_array().expect("peerings");
    assert_eq!(peerings.len(), 2, "{report}");
    assert_eq!(
        (peerings[0]["from"].as_str(), peerings[0]["to"].as_str()),
        (Some("second"), Some("relay"))
    );
    assert_eq!(
        (peerings[1]["from"].as_str(), peerings[1]["to"].as_str()),
        (Some("relay"), Some("second"))
    );
    // Each peering is paid on a channel of its own, from the key of the connector that peers.
    assert_eq!(
        chain.balance(&address),
        DEPOSIT * 9,
        "the deposit is on chain"
    );

    // The wallet lists the new connector's address, which is the one that was funded.
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]).json();
    assert_eq!(
        shown["wallet"]["chains"]["evm"][1]["address"],
        address.as_str()
    );
    let status = machine.toon(&["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{}", status.stdout);

    // First to second: the app behind the new connector gets the packet.
    let second = format!("http://{}/ilp", listening(&machine, "second"));
    let second_segment = machine.segment("second");
    assert_ne!(second_segment, machine.segment("relay"));
    assert_eq!(second_segment.len(), 16);
    let second_app = format!("g.toon.{second_segment}.second");
    assert_eq!(
        report["created"]["address"],
        format!("g.toon.{second_segment}")
    );
    // The source forwards the new connector's address to it.
    let routes = machine.toon(&["route", "list", "--json"]).json();
    assert!(
        routes["forwarding_routes"]
            .as_array()
            .expect("the forwarding routes")
            .iter()
            .any(
                |route| route["prefix"] == format!("g.toon.{second_segment}").as_str()
                    && route["peer_id"] == "second"
            ),
        "{routes}"
    );
    let sent = machine.toon(&[
        "send",
        &second_app,
        "--amount",
        "0",
        "--yes",
        "--seal-to",
        &second,
        "--json",
    ]);
    assert_eq!(sent.json()["outcome"], "fulfilled", "{}", sent.stdout);
    assert_eq!(sent.exit_code, 0);
    assert_eq!(writes(&machine, "second").len(), 1);

    // Second to first: the relay's price is paid.
    let first = format!("http://{}/ilp", listening(&machine, "relay"));
    let relay = machine.relay_prefix();
    let back = machine.toon(&[
        "send",
        "--app",
        "second",
        &relay,
        "--amount",
        "1",
        "--yes",
        "--seal-to",
        &first,
        "--json",
    ]);
    assert_eq!(back.json()["outcome"], "fulfilled", "{}", back.stdout);
    // It reached the relay, which refuses a write that carries no event.
    assert_eq!(
        back.json()["response"]["body"],
        r#"{"error":"Invalid request body"}"#
    );

    // Every other command takes the new name: its peerings are its own.
    let peers = machine
        .toon(&["peer", "list", "--app", "second", "--json"])
        .json();
    assert_eq!(peers["peers"][0]["id"], "relay", "{peers}");
    let peers = machine.toon(&["peer", "list", "--json"]).json();
    assert_eq!(peers["peers"][0]["id"], "second", "{peers}");
}

#[test]
fn destroy_refuses_while_a_channel_holds_funds_and_names_it() {
    let chain = AnvilChain::start();
    let (machine, _up) = running(&chain);
    let deposit = DEPOSIT.to_string();
    let refused = create_second(&machine, &["--deposit", &deposit, "--yes"]);
    let address = address_in(refused.json()["error"]["message"].as_str().unwrap());
    chain.fund(&address, DEPOSIT * 10);
    assert_eq!(
        create_second(&machine, &["--deposit", &deposit, "--yes"]).exit_code,
        0
    );
    let channels = machine
        .toon(&["channel", "list", "--app", "second", "--json"])
        .json();
    let id = channels["channels"]
        .as_array()
        .and_then(|channels| channels.iter().find(|c| c["direction"] == "outbound"))
        .and_then(|channel| channel["id"].as_str())
        .expect("an outbound channel")
        .to_owned();

    let run = machine.toon(&["destroy", "second", "--json"]);

    assert_eq!(run.json()["error"]["code"], "funds_held", "{}", run.stdout);
    assert!(
        run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&id),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn destroy_stops_and_removes_a_toon_app_that_holds_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    let created = with_passphrase(
        &machine,
        &[
            "create",
            "second",
            "--image",
            "second:1",
            "--no-peer",
            "--clearnet",
            "second.example.com",
            "--json",
        ],
    );
    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    assert_eq!(created.json()["started"], true);
    assert_eq!(created.json()["peerings"], Value::Array(vec![]));
    let address = listening(&machine, "second");
    assert!(address.starts_with("127.0.0.1:"), "{address}");
    let files = machine.agent_node_home().join("connectors").join("1");
    assert!(files.join("identity.key").exists());

    let run = machine.toon(&["destroy", "second", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["destroyed"], "second");
    let status = machine.toon(&["status", "--json"]).json();
    assert_eq!(
        status["agent_node"]["toon_apps"].as_array().map(Vec::len),
        Some(1),
        "{status}"
    );
    assert!(!files.join("identity.key").exists());
    let again = machine.toon(&["destroy", "second", "--json"]);
    assert_eq!(again.json()["error"]["code"], "unknown_name");
    // The connector of the first TOON app was not touched, and the agent node always has one.
    let last = machine.toon(&["destroy", "relay", "--json"]);
    assert_eq!(last.json()["error"]["code"], "last_toon_app");
    // The keys of a destroyed TOON app are not given to the next one.
    let next = with_passphrase(
        &machine,
        &[
            "create",
            "third",
            "--image",
            "third:1",
            "--no-peer",
            "--clearnet",
            "third.example.com",
            "--json",
        ],
    );
    assert_eq!(next.exit_code, 0, "{}", next.stdout);
    assert_eq!(next.json()["created"]["connector"], 2);
}

#[test]
fn a_toon_app_is_created_while_the_agent_node_is_down_and_up_starts_it() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let created = with_passphrase(
        &machine,
        &[
            "create",
            "second",
            "--url",
            "http://127.0.0.1:9/",
            "--no-peer",
            "--clearnet",
            "second.example.com",
            "--json",
        ],
    );

    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    assert_eq!(created.json()["started"], false);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    assert!(listening(&machine, "second").starts_with("127.0.0.1:"));
    // A name the agent node has is not taken twice, by a TOON app or by an app.
    for taken in ["second", "relay"] {
        let run = with_passphrase(
            &machine,
            &[
                "create",
                taken,
                "--image",
                "x:1",
                "--no-peer",
                "--clearnet",
                "x.example.com",
                "--json",
            ],
        );
        assert_eq!(run.json()["error"]["code"], "name_taken", "{taken}");
    }
}

#[test]
fn create_needs_a_deposit_or_no_peering_and_a_yes_for_the_deposit() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let neither = with_passphrase(&machine, &["create", "second", "--image", "x:1", "--json"]);
    let unconfirmed = with_passphrase(
        &machine,
        &[
            "create",
            "second",
            "--image",
            "x:1",
            "--deposit",
            "5",
            "--clearnet",
            "second.example.com",
            "--json",
        ],
    );
    let unknown = with_passphrase(
        &machine,
        &[
            "create",
            "second",
            "--image",
            "x:1",
            "--no-peer",
            "--app",
            "nowhere",
            "--clearnet",
            "second.example.com",
            "--json",
        ],
    );

    assert_eq!(neither.json()["error"]["code"], "usage");
    assert_eq!(neither.exit_code, 2);
    assert_eq!(unconfirmed.json()["error"]["code"], "not_confirmed");
    assert_eq!(unknown.json()["error"]["code"], "unknown_name");
    let state = fs::read_to_string(machine.agent_node_home().join("state.json")).unwrap();
    assert!(!state.contains("second"), "{state}");
}
