//! The token is needed where it is spent: before a command that moves it from the wallet into
//! a channel, not before `toon up`.

mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::fake_solana::FakeSolana;
use support::{Foreground, Machine};

const SOLANA_FEES: u64 = 10_000_000;

/// A node whose chain holds gas and `token` base units of the token, with its connector
/// running.
fn running(token: u128) -> (Machine, FakeChain, String, Foreground) {
    let chain = FakeChain::start_with_token(token);
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

fn assert_refused_for_token(run: &support::Run, address: &str, amount: &str) {
    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let error = &run.json()["error"];
    assert_eq!(error["code"], "unfunded", "{error}");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(address), "{message}");
    assert!(
        message.contains(&format!("{amount} base units")),
        "{message}"
    );
}

fn init_on_solana(machine: &Machine, evm: &FakeChain, solana: &FakeSolana) {
    let args = [
        "--evm-rpc-url".to_owned(),
        evm.rpc_url(),
        "--evm-token".to_owned(),
        support::fake_chain::TOKEN.to_owned(),
        "--evm-decimals".to_owned(),
        "6".to_owned(),
        "--evm-transfer-method".to_owned(),
        "permit2".to_owned(),
        "--solana".to_owned(),
        "--solana-rpc-url".to_owned(),
        solana.rpc_url(),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_eq!(machine.init_with(&args).exit_code, 0);
}

#[test]
fn up_starts_on_an_evm_key_that_holds_no_token() {
    // `running` has already read the report of a connector that is listening.
    let (machine, _chain, _address, _up) = running(0);
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 0);
}

#[test]
fn up_starts_on_a_solana_key_that_holds_the_fees_and_no_token() {
    let (evm, solana) = (
        FakeChain::start_tokenless(),
        FakeSolana::start_with(SOLANA_FEES, 0),
    );
    let machine = Machine::new();
    init_on_solana(&machine, &evm, &solana);

    let up = machine.start(&["up", "--foreground", "--json"]);

    let report = up.report();
    let status = machine.toon(&["status", "--json"]);
    assert_eq!(status.exit_code, 0, "{report}\n{}", status.stdout);
}

#[test]
fn up_on_a_solana_key_that_holds_less_than_the_fees_is_unfunded() {
    let (evm, solana) = (
        FakeChain::start(),
        FakeSolana::start_with(SOLANA_FEES - 1, 5_000_000),
    );
    let machine = Machine::new();
    init_on_solana(&machine, &evm, &solana);

    let up = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(up.exit_code, 1, "{}", up.stdout);
    let error = &up.json()["error"];
    assert_eq!(error["code"], "unfunded");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("0.01 SOL"), "{message}");
    assert!(!message.contains("1 token"), "{message}");
}

#[test]
fn join_and_peer_add_refuse_without_the_deposit_and_charge_nothing() {
    let (machine, _chain, address, _up) = running(999);
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
        assert_refused_for_token(&machine.toon(&args), &address, "1000");
    }

    assert_eq!(state(&machine), before);
    assert_eq!(machine.toon(&["limit", "show", "--json"]).json(), limits);
}

#[test]
fn channel_open_and_fund_refuse_without_the_amount() {
    let (machine, _chain, address, _up) = running(4);
    let terms = machine.home().join("terms.json");
    fs::write(&terms, "{}").unwrap();
    let terms = terms.to_str().unwrap().to_owned();

    let open = machine.toon(&[
        "channel",
        "open",
        "--terms",
        &terms,
        "--deposit",
        "5",
        "--json",
    ]);
    let fund = machine.toon(&["channel", "fund", "0xab", "--amount", "5", "--json"]);

    assert_refused_for_token(&open, &address, "5");
    assert_refused_for_token(&fund, &address, "5");
}

#[test]
fn create_with_a_deposit_needs_twice_the_deposit_in_the_source_key() {
    let (machine, _chain, address, _up) = running(1999);
    let before = state(&machine);
    let create = |deposit: &str| {
        machine.toon_with(
            &[
                "create",
                "second",
                "--image",
                "second:1",
                "--clearnet",
                "second.example.com",
                "--deposit",
                deposit,
                "--yes",
                "--json",
            ],
            |command| {
                command.env("TOON_PASSPHRASE", support::PASSPHRASE);
            },
        )
    };

    let refused = create("1000");

    assert_refused_for_token(&refused, &address, "2000");
    assert_eq!(state(&machine), before);
}

#[test]
fn create_without_a_deposit_succeeds_when_the_new_key_holds_no_token() {
    let (machine, _chain, _address, _up) = running(0);

    let created = machine.toon_with(
        &[
            "create",
            "second",
            "--image",
            "second:1",
            "--clearnet",
            "second.example.com",
            "--no-peer",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(created.exit_code, 0, "{}", created.stdout);
}

#[test]
fn deposits_of_what_the_wallet_holds_pass_the_check() {
    let (machine, _chain, _address, _up) = running(2000);
    let terms = machine.home().join("terms.json");
    fs::write(&terms, "{}").unwrap();
    let terms = terms.to_str().unwrap().to_owned();

    let open = machine.toon(&[
        "channel",
        "open",
        "--terms",
        &terms,
        "--deposit",
        "2000",
        "--json",
    ]);
    let fund = machine.toon(&["channel", "fund", "0xab", "--amount", "2000", "--json"]);
    let create = machine.toon_with(
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

    // The fake chain takes no transaction, so the connector refuses what comes after.
    for run in [open, fund, create] {
        assert_ne!(run.json()["error"]["code"], "unfunded", "{}", run.stdout);
    }
}

#[test]
fn a_deposit_whose_chain_the_connector_chooses_is_not_checked() {
    let (evm, solana) = (
        FakeChain::start_tokenless(),
        FakeSolana::start_with(SOLANA_FEES, 0),
    );
    let machine = Machine::new();
    init_on_solana(&machine, &evm, &solana);

    let fund = machine.toon(&["channel", "fund", "0xab", "--amount", "5", "--json"]);

    // Past the check, nothing runs to send it to.
    assert_eq!(
        fund.json()["error"]["code"],
        "not_running",
        "{}",
        fund.stdout
    );
}

#[test]
fn a_chain_that_cannot_be_asked_is_not_a_verdict() {
    let chain = FakeChain::start_tokenless();
    let machine = Machine::new();
    assert_eq!(machine.init_on_clearnet(&chain).exit_code, 0);
    drop(chain);

    let fund = machine.toon(&["channel", "fund", "0xab", "--amount", "5", "--json"]);

    assert_eq!(
        fund.json()["error"]["code"],
        "not_running",
        "{}",
        fund.stdout
    );
}
