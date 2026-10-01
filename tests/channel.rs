mod support;

use support::fake_chain::{FakeChain, NATIVE_BALANCE, TOKEN, TOKEN_BALANCE};
use support::local_chain::LocalChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

fn with_passphrase(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

/// An agent node on the fake chain, running, with the fake relay behind its connector.
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
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    // `init` always records a chain now: an agent node with none is one from before it did.
    let recorded = std::fs::read(machine.agent_node_home().join("state.json")).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&recorded).unwrap();
    state["toon_apps"][0]["evm"] = serde_json::Value::Null;
    machine.write_agent_node_file("state.json", state.to_string());

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

/// An agent node on a local chain, running, whose settlement account holds `FUNDED`.
struct OnChain {
    machine: Machine,
    _up: Foreground,
    /// Where its connector listens.
    connector: String,
    /// Its settlement account.
    address: String,
}

/// What each agent node's settlement account holds of the USDC to start with.
const FUNDED: u128 = 1_000_000;

fn on_chain(chain: &LocalChain) -> OnChain {
    let machine = Machine::new();
    let init = machine.init_on_local(chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]).json();
    let address = shown["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("an EVM address")
        .to_owned();
    chain.fund(&address, FUNDED);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let connector = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .to_owned();
    OnChain {
        machine,
        _up: up,
        connector,
        address,
    }
}

impl OnChain {
    /// The `batchSettlements` entry this agent node's connector publishes, as a file the
    /// other machine can name.
    fn terms_for(&self, other: &Machine) -> std::path::PathBuf {
        let described: serde_json::Value =
            reqwest::blocking::get(format!("http://{}/ilp", self.connector))
                .and_then(|response| response.json())
                .expect("the connector's self-description");
        let terms = described["batchSettlements"][0].clone();
        assert!(terms.is_object(), "{described}");
        other.write_agent_node_file("terms.json", terms.to_string())
    }
}

#[test]
fn an_outbound_channel_is_opened_funded_listed_and_withdrawn() {
    if !LocalChain::available() {
        return;
    }
    let chain = LocalChain::start();
    let payee = on_chain(&chain);
    let payer = on_chain(&chain);
    let terms = payee.terms_for(&payer.machine);
    let withdraw_delay: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&terms).unwrap()).unwrap();
    let withdraw_delay = withdraw_delay["withdrawDelay"].as_u64().expect("a delay");

    let opened = payer.machine.toon(&[
        "channel",
        "open",
        "--terms",
        terms.to_str().unwrap(),
        "--deposit",
        "1000",
        "--json",
    ]);

    let channel = &opened.json()["channel"];
    assert_eq!(channel["direction"], "outbound", "{channel}");
    assert_eq!(channel["status"], "open");
    assert_eq!(channel["collateral"], 1000);
    assert_eq!(channel["resumed"], false);
    assert_eq!(opened.exit_code, 0);
    let id = channel["id"].as_str().expect("an id").to_owned();
    assert_eq!(chain.balance(&payer.address), FUNDED - 1000);

    let funded = payer
        .machine
        .toon(&["channel", "fund", &id, "--amount", "500"]);

    assert!(
        funded
            .stdout
            .starts_with(&format!("Funded with 500: {id} evm outbound open")),
        "{}",
        funded.stdout
    );
    assert!(
        funded.stdout.contains("collateral 1500"),
        "{}",
        funded.stdout
    );
    assert_eq!(funded.exit_code, 0);
    assert_eq!(chain.balance(&payer.address), FUNDED - 1500);

    let listed = payer.machine.toon(&["channel", "list", "--json"]);

    let channels = listed.json()["channels"].clone();
    assert_eq!(channels.as_array().map(Vec::len), Some(1), "{channels}");
    assert_eq!(channels[0]["id"], id.as_str());
    assert_eq!(channels[0]["direction"], "outbound");
    assert_eq!(channels[0]["collateral"], 1500);
    assert_eq!(channels[0]["status"], "open");

    let started = payer.machine.toon(&["channel", "withdraw", &id, "--json"]);

    let channel = &started.json()["channel"];
    assert_eq!(channel["step"], "started", "{channel}");
    assert_eq!(channel["status"], "withdrawing");
    assert_eq!(started.exit_code, 0);

    chain.advance_time(withdraw_delay + 1);
    // A write signed in the same second as an identical one is the same signature, which
    // the connector refuses as replayed.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let finished = payer.machine.toon(&["channel", "withdraw", &id]);

    assert!(
        finished.stdout.starts_with("Withdrawal step finished: "),
        "{}{}",
        finished.stdout,
        finished.stderr
    );
    assert_eq!(finished.exit_code, 0);
    assert_eq!(chain.balance(&payer.address), FUNDED);
    let balances = with_passphrase(&payer.machine, &["wallet", "balances", "--json"]).json();
    assert_eq!(
        balances["balances"][0]["token"]["balance"],
        FUNDED.to_string()
    );
}

#[test]
fn land_is_refused_on_a_channel_this_node_pays() {
    if !LocalChain::available() {
        return;
    }
    let chain = LocalChain::start();
    let payee = on_chain(&chain);
    let payer = on_chain(&chain);
    let terms = payee.terms_for(&payer.machine);
    let opened = payer.machine.toon(&[
        "channel",
        "open",
        "--terms",
        terms.to_str().unwrap(),
        "--deposit",
        "1000",
        "--json",
    ]);
    let id = opened.json()["channel"]["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let run = payer.machine.toon(&["channel", "land", &id, "--json"]);

    let report = run.json();
    assert_eq!(report["error"]["code"], "channel_failed", "{report}");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn an_amount_above_u64_reaches_the_connector() {
    let node = running();
    let unknown = format!("0x{}", "ab".repeat(32));
    let amount = (u128::from(u64::MAX) + 1).to_string();

    let run = node
        .machine
        .toon(&["channel", "fund", &unknown, "--amount", &amount, "--json"]);

    assert_eq!(
        run.json()["error"]["code"],
        "channel_failed",
        "{}",
        run.stderr
    );
    assert_eq!(run.exit_code, 1);
}
