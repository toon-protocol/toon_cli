mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine, PASSPHRASE};

/// An agent node, running, with the fake relay behind its connector.
struct Running {
    machine: Machine,
    _up: Foreground,
    _chain: FakeChain,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
    }
}

fn set_limits(machine: &Machine, per_command: &str, per_day: &str) {
    let run = machine.toon_with(
        &[
            "limit",
            "set",
            "--max-per-command",
            per_command,
            "--max-per-day",
            per_day,
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
}

fn send(machine: &Machine, amount: &str) -> support::Run {
    machine.toon(&[
        "send",
        &machine.relay_address(),
        "--amount",
        amount,
        "--yes",
        "--json",
    ])
}

#[test]
fn a_command_that_moves_money_needs_yes() {
    let node = running();
    let relay = node.machine.relay_address();

    for args in [
        &["send", &relay, "--amount", "1", "--json"][..],
        &[
            "peer",
            "add",
            "http://127.0.0.1:1/ilp",
            "--deposit",
            "1",
            "--json",
        ],
    ] {
        let run = node.machine.toon(args);
        assert_eq!(run.json()["error"]["code"], "not_confirmed", "{args:?}");
        assert_eq!(run.exit_code, 1);
    }
}

#[test]
fn a_payment_under_the_limit_is_made_and_one_over_it_is_refused() {
    let node = running();
    set_limits(&node.machine, "5", "8");

    let paid = send(&node.machine, "5");
    assert_eq!(paid.json()["outcome"], "fulfilled", "{}", paid.stdout);
    assert_eq!(paid.exit_code, 0);

    let too_much = send(&node.machine, "6");
    let error = too_much.json()["error"].clone();
    assert_eq!(error["code"], "spending_limit", "{error}");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("per-command spending limit of 5"),
        "{message}"
    );
    assert_eq!(too_much.exit_code, 1);

    let over_the_day = send(&node.machine, "4");
    let message = over_the_day.json()["error"]["message"].clone();
    let message = message.as_str().unwrap_or_default();
    assert!(message.contains("3 of 8 remains today"), "{message}");

    let shown = node.machine.toon(&["limit", "show", "--json"]).json();
    assert_eq!(shown["limits"]["remaining_today"], "3");
    assert_eq!(send(&node.machine, "3").exit_code, 0);
}

#[test]
fn a_payment_that_did_not_happen_is_not_counted() {
    let node = running();
    set_limits(&node.machine, "5", "5");

    let rejected =
        node.machine
            .toon(&["send", "g.nobody.here", "--amount", "5", "--yes", "--json"]);
    assert_eq!(
        rejected.json()["outcome"],
        "rejected",
        "{}",
        rejected.stdout
    );

    let shown = node.machine.toon(&["limit", "show", "--json"]).json();
    assert_eq!(shown["limits"]["remaining_today"], "5");
}

#[test]
fn limits_are_set_at_init_and_changed_only_with_the_passphrase() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--max-per-command",
        "7",
        "--max-per-day",
        "9",
    ]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon(&["limit", "show", "--json"]).json();
    assert_eq!(shown["limits"]["per_command"], "7");
    assert_eq!(shown["limits"]["per_day"], "9");

    let no_passphrase = machine.toon(&["limit", "set", "--max-per-day", "99", "--json"]);
    assert_eq!(no_passphrase.json()["error"]["code"], "passphrase_missing");
    let wrong = machine.toon_with(&["limit", "set", "--max-per-day", "99", "--json"], |c| {
        c.env("TOON_PASSPHRASE", "not it");
    });
    assert_eq!(wrong.json()["error"]["code"], "passphrase_wrong");

    // Setting one limit leaves the other.
    set_limits(&machine, "70", "90");
    let shown = machine.toon(&["limit", "show", "--json"]).json();
    assert_eq!(shown["limits"]["per_day"], "90");
}

#[test]
fn an_edited_limit_stops_every_payment() {
    let node = running();
    let file = node.machine.agent_node_home().join("limits.json");
    let text = fs::read_to_string(&file).unwrap();
    let edited = text.replace(
        "\"per_command\": \"10000000\"",
        "\"per_command\": \"99999999999\"",
    );
    assert_ne!(text, edited);
    fs::write(&file, edited).unwrap();

    let run = send(&node.machine, "1");

    assert_eq!(
        run.json()["error"]["code"],
        "spending_limit",
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
}

#[test]
fn a_limit_that_is_gone_is_set_again_only_whole() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    fs::remove_file(machine.agent_node_home().join("limits.json")).unwrap();

    let one = machine.toon_with(&["limit", "set", "--max-per-day", "9", "--json"], |c| {
        c.env("TOON_PASSPHRASE", PASSPHRASE);
    });
    assert_eq!(
        one.json()["error"]["code"],
        "spending_limit",
        "{}",
        one.stdout
    );
    assert_eq!(one.exit_code, 1);

    set_limits(&machine, "7", "9");
    let shown = machine.toon(&["limit", "show", "--json"]).json();
    assert_eq!(shown["limits"]["per_command"], "7");
    assert_eq!(shown["limits"]["per_day"], "9");
}
