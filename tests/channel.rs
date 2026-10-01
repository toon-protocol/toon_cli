mod support;

use support::fake_chain::{FakeChain, NATIVE_BALANCE, TOKEN, TOKEN_BALANCE};
use support::stub_app::StubApp;
use support::{Foreground, Machine, Run, PASSPHRASE};

fn with_passphrase(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

/// An agent node on the fake chain, running.
struct Running {
    machine: Machine,
    _up: Foreground,
    _chain: FakeChain,
    _app: StubApp,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let app = StubApp::start();
    let machine = Machine::new();
    let init = machine.init_on_serving(&chain, app.url());
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--json"]);
    up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
        _app: app,
    }
}

#[test]
fn balances_show_every_address_by_toon_app_and_chain() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]).json();

    let run = with_passphrase(&machine, &["wallet", "balances", "--json"]);

    let report = run.json();
    let balances = report["balances"].as_array().expect("balances");
    assert_eq!(balances.len(), 2, "{report}");
    let evm = &balances[0];
    assert_eq!(evm["toon_app"], "relay");
    assert_eq!(evm["chain"], "evm");
    assert_eq!(
        evm["address"],
        shown["wallet"]["chains"]["evm"][0]["address"]
    );
    assert_eq!(evm["native"], NATIVE_BALANCE.to_string());
    assert_eq!(evm["token"]["address"], TOKEN);
    assert_eq!(evm["token"]["balance"], TOKEN_BALANCE.to_string());
    let solana = &balances[1];
    assert_eq!(solana["chain"], "solana");
    assert_eq!(
        solana["address"],
        shown["wallet"]["chains"]["solana"][0]["address"]
    );
    assert_eq!(solana["native"], serde_json::Value::Null);
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
}

#[test]
fn balances_in_text_name_the_app_the_chain_and_the_address() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = with_passphrase(&machine, &["wallet", "balances"]);

    assert!(
        run.stdout.starts_with("relay evm 0x")
            && run
                .stdout
                .contains(&format!("{TOKEN_BALANCE} of token {TOKEN}")),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn balances_without_a_chain_have_nothing_to_read() {
    let machine = Machine::new();
    let init = with_passphrase(&machine, &["init", "--json"]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let run = with_passphrase(&machine, &["wallet", "balances", "--json"]);

    let report = run.json();
    assert_eq!(report["balances"][0]["native"], serde_json::Value::Null);
    assert_eq!(report["balances"][0]["token"], serde_json::Value::Null);
    assert_eq!(run.exit_code, 0);
}

#[test]
fn balances_on_an_unreachable_chain_say_so() {
    let machine = Machine::new();
    let chain = FakeChain::start();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    drop(chain);

    let run = with_passphrase(&machine, &["wallet", "balances", "--json"]);

    assert_eq!(run.json()["error"]["code"], "chain_failed");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn balances_on_a_machine_with_no_wallet_say_so() {
    let machine = Machine::new();

    let run = with_passphrase(&machine, &["wallet", "balances", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_wallet");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn a_new_connector_holds_no_channels() {
    let node = running();

    let run = node.machine.toon(&["channel", "list", "--json"]);

    assert_eq!(run.json(), serde_json::json!({ "channels": [] }));
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
    let text = node.machine.toon(&["channel", "list"]);
    assert_eq!(text.stdout, "The connector has no channels.\n");
}

#[test]
fn channel_commands_need_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    for args in [
        &["channel", "list", "--json"][..],
        &["channel", "fund", "0xab", "--amount", "1", "--json"],
        &["channel", "withdraw", "0xab", "--json"],
        &["channel", "land", "0xab", "--json"],
    ] {
        let run = machine.toon(args);

        assert_eq!(run.json()["error"]["code"], "not_running", "{args:?}");
        assert_eq!(run.exit_code, 1);
    }
}

#[test]
fn channel_commands_on_a_machine_with_no_agent_node_say_so() {
    let machine = Machine::new();

    let run = machine.toon(&["channel", "list", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn the_connector_refuses_a_write_on_a_channel_it_does_not_hold() {
    let node = running();
    let unknown = format!("0x{}", "ab".repeat(32));

    for (args, status) in [
        (vec!["channel", "fund", &unknown, "--amount", "5"], "400"),
        (vec!["channel", "withdraw", &unknown], "404"),
        (vec!["channel", "land", &unknown], "404"),
    ] {
        let mut args = args;
        args.push("--json");
        let run = node.machine.toon(&args);

        let report = run.json();
        assert_eq!(
            report["error"]["code"], "channel_failed",
            "{args:?}: {report}"
        );
        let message = report["error"]["message"].as_str().unwrap();
        assert!(message.contains(status), "{args:?}: {message}");
        assert!(message.contains("channel"), "{message}");
        assert_eq!(run.exit_code, 1);
        assert_eq!(run.stderr, "");
    }
}

#[test]
fn a_channel_id_is_not_a_path() {
    let node = running();

    let run = node
        .machine
        .toon(&["channel", "land", "../peers", "--json"]);

    assert_eq!(run.json()["error"]["code"], "channel_failed");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn open_reports_terms_the_connector_does_not_accept() {
    let node = running();
    let terms = node.machine.write_agent_node_file("terms.json", "{}");

    let run = node.machine.toon(&[
        "channel",
        "open",
        "--terms",
        terms.to_str().unwrap(),
        "--deposit",
        "1000",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["error"]["code"], "channel_failed", "{report}");
    assert!(report["error"]["message"].as_str().unwrap().contains("400"));
    assert_eq!(run.exit_code, 1);
}

#[test]
fn open_reports_a_terms_file_that_is_not_there() {
    let node = running();

    let run = node.machine.toon(&[
        "channel",
        "open",
        "--terms",
        "/nonexistent/terms.json",
        "--deposit",
        "1000",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "channel_failed");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn open_reports_a_terms_file_that_is_not_json() {
    let node = running();
    let terms = node.machine.write_agent_node_file("terms.json", "not json");

    let run = node.machine.toon(&[
        "channel",
        "open",
        "--terms",
        terms.to_str().unwrap(),
        "--deposit",
        "1000",
    ]);

    assert!(run.stderr.contains("not JSON"), "{}", run.stderr);
    assert_eq!(run.exit_code, 1);
}

#[test]
fn fund_needs_an_amount() {
    let machine = Machine::new();

    let run = machine.toon(&["channel", "fund", "0xab", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}
