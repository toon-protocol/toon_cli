//! An EVM settlement key needs gas where gas is spent: not before `toon up`, but before a
//! command that has the connector send a transaction.

mod support;

use std::fs;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// A node whose chain holds the token and no gas, with its connector running.
fn gasless() -> (Machine, FakeChain, String, Foreground) {
    let chain = FakeChain::start_gasless();
    let machine = Machine::new();
    let init = machine.init_on_clearnet(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let address = init.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    (machine, chain, address, up)
}

fn state(machine: &Machine) -> Vec<u8> {
    fs::read(machine.agent_node_home().join("state.json")).unwrap_or_default()
}

fn assert_refused_for_gas(run: &support::Run, address: &str) {
    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let error = &run.json()["error"];
    assert_eq!(error["code"], "unfunded", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(address), "{message}");
    assert!(message.contains("0.0001 ETH"), "{message}");
    assert!(!message.contains("wallet fund"), "{message}");
    assert!(message.contains("Base Sepolia"), "{message}");
}

#[test]
fn up_starts_on_a_key_that_holds_the_token_and_no_gas() {
    // `gasless` has already read the report of a connector that is listening.
    let (machine, _chain, _address, _up) = gasless();
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 0);
}

#[test]
fn up_on_a_key_that_holds_nothing_asks_for_the_token_and_no_eth() {
    let chain = FakeChain::start_unfunded();
    let machine = Machine::new();
    machine.init_on(&chain);

    let up = machine.toon(&["up", "--foreground", "--json"]);

    let error = &up.json()["error"];
    assert_eq!(error["code"], "unfunded");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("1000000 base units"), "{message}");
    assert!(!message.contains("ETH"), "{message}");
    assert!(message.contains("toon wallet fund"), "{message}");
}

#[test]
fn join_and_peer_add_refuse_without_gas_and_charge_nothing() {
    let (machine, _chain, address, _up) = gasless();
    let before = state(&machine);
    let limits = machine.toon(&["limit", "show", "--json"]).json();

    for args in [
        vec!["join", "devnet", "--deposit", "1000", "--yes", "--json"],
        vec![
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "1000",
            "--yes",
            "--json",
        ],
    ] {
        let run = machine.toon(&args);
        assert_refused_for_gas(&run, &address);
    }

    assert_eq!(state(&machine), before);
    assert_eq!(machine.toon(&["limit", "show", "--json"]).json(), limits);
}

#[test]
fn every_channel_write_refuses_without_gas() {
    let (machine, _chain, address, _up) = gasless();
    let terms = machine.home().join("terms.json");
    fs::write(&terms, "{}").unwrap();
    let terms = terms.to_str().unwrap().to_owned();

    for args in [
        vec![
            "channel",
            "open",
            "--terms",
            &terms,
            "--deposit",
            "5",
            "--json",
        ],
        vec!["channel", "fund", "0xab", "--amount", "1", "--json"],
        vec!["channel", "withdraw", "0xab", "--json"],
        vec!["channel", "land", "0xab", "--json"],
    ] {
        assert_refused_for_gas(&machine.toon(&args), &address);
    }
}

#[test]
fn create_with_a_deposit_refuses_without_gas_and_creates_nothing() {
    let (machine, _chain, address, _up) = gasless();
    let before = state(&machine);

    let run = machine.toon_with(
        &[
            "create",
            "second",
            "--image",
            "second:1",
            "--clearnet",
            "second.example.com",
            "--deposit",
            "1000",
            "--yes",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_refused_for_gas(&run, &address);
    assert_eq!(state(&machine), before);
    assert!(!machine.agent_node_home().join("connectors/1").exists());
}

#[test]
fn create_without_a_peering_needs_no_gas() {
    let (machine, _chain, _address, _up) = gasless();

    let run = machine.toon_with(
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
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["started"], Value::Bool(true));
}
