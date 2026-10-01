mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::fake_faucet::FakeFaucet;
use support::Machine;

fn config(machine: &Machine) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors")
            .join("0")
            .join("connector.toml"),
    )
    .expect("read the rendered connector config")
}

#[test]
fn init_defaults_to_the_devnet_and_leaves_solana_off() {
    let machine = Machine::new();

    let init = machine.init_with(&[]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(init.json()["network"], "devnet");
    let config = config(&machine);
    assert!(config.contains(r#"rpc_url = "https://base-sepolia-rpc.publicnode.com""#));
    assert!(config.contains(r#"token_address = "0x0C996d7c934c79a6255254875607Fe69df25C0E1""#));
    assert!(config.contains(r#"asset_eip712_name = "USDC""#));
    assert!(!config.contains("settlement.solana"), "{config}");
    let node = machine.agent_node_home().join("connectors/0");
    assert!(!node.join("settlement-solana.key").exists());
}

#[test]
fn the_sandbox_profile_renders_the_local_chain() {
    let machine = Machine::new();

    let init = machine.init_with(&["--network", "sandbox"]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(init.json()["network"], "sandbox");
    let config = config(&machine);
    assert!(
        config.contains(r#"rpc_url = "http://localhost:8545""#),
        "{config}"
    );
    assert!(config.contains(r#"token_address = "0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512""#));
    assert!(!config.contains("settlement.solana"));
}

#[test]
fn the_mainnet_profile_renders_base_and_the_tokens_own_name() {
    let machine = Machine::new();

    let init = machine.init_with(&["--network", "mainnet"]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let config = config(&machine);
    assert!(
        config.contains(r#"rpc_url = "https://mainnet.base.org""#),
        "{config}"
    );
    assert!(config.contains(r#"token_address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913""#));
    assert!(config.contains(r#"asset_eip712_name = "USD Coin""#));
}

#[test]
fn solana_settles_only_when_the_operator_opts_in() {
    for (network, rpc, mint) in [
        (
            "devnet",
            "https://api.devnet.solana.com",
            "34eSxY7qxQ4GzyhDJ8GpUcTz1WWzruGbJbR8q6TtxfQU",
        ),
        (
            "sandbox",
            "http://localhost:8899",
            "H8HSreUF2s8r8hem4qMttE3bWYCpFuh71jbuos5bA77H",
        ),
        (
            "mainnet",
            "https://api.mainnet-beta.solana.com",
            "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
        ),
    ] {
        let machine = Machine::new();

        let init = machine.init_with(&["--network", network, "--solana"]);

        assert_eq!(init.exit_code, 0, "{network}: {}", init.stdout);
        let config = config(&machine);
        assert!(
            config.contains("[settlement.solana]"),
            "{network}: {config}"
        );
        assert!(
            config.contains(&format!(r#"rpc_url = "{rpc}""#)),
            "{network}: {config}"
        );
        assert!(config.contains(&format!(r#"token_address = "{mint}""#)));
        assert!(config.contains("min_sponsored_deposit = 1000000"));
        let key = machine
            .agent_node_home()
            .join("connectors/0/settlement-solana.key");
        assert_eq!(fs::read(key).unwrap().len(), 32);
        let solana_needs = init.json()["needs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|need| need["chain"] == "solana")
            .count();
        assert_eq!(solana_needs, 2, "{network}");
    }
}

#[test]
fn a_flag_replaces_one_setting_of_the_profile() {
    let machine = Machine::new();

    let init = machine.init_with(&[
        "--evm-rpc-url",
        "http://127.0.0.1:1",
        "--evm-decimals",
        "18",
    ]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let config = config(&machine);
    assert!(config.contains(r#"rpc_url = "http://127.0.0.1:1""#));
    assert!(config.contains("decimals = 18"));
    assert!(config.contains(r#"token_address = "0x0C996d7c934c79a6255254875607Fe69df25C0E1""#));
}

#[test]
fn init_says_which_address_needs_how_much_and_how_to_fund_it() {
    let machine = Machine::new();

    let init = machine.init_with(&[]);

    let report = init.json();
    let address = report["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .unwrap();
    let needs = report["needs"].as_array().unwrap();
    assert_eq!(needs.len(), 2);
    assert!(needs.iter().all(|need| need["address"] == address));
    assert_eq!(needs[0]["amount"], "100000000000000");
    assert_eq!(needs[1]["amount"], "1000000");
    let text = fresh_text_init();
    assert!(text.contains("0.0001 ETH"), "{text}");
    assert!(text.contains("toon wallet fund"), "{text}");
}

fn fresh_text_init() -> String {
    let machine = Machine::new();
    let run = machine.toon_with(&["init", "--accept-anyone-terms"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    run.stdout
}

#[test]
fn mainnet_init_tells_the_operator_to_fund_the_addresses_themselves() {
    let machine = Machine::new();
    let run = machine.toon_with(
        &["init", "--accept-anyone-terms", "--network", "mainnet"],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("fund them yourself"), "{}", run.stdout);
    assert!(!run.stdout.contains("wallet fund"), "{}", run.stdout);
}

#[test]
fn up_does_not_start_a_connector_whose_settlement_key_is_unfunded() {
    let chain = FakeChain::start_unfunded();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    let address = init.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .unwrap()
        .to_owned();

    let up = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(up.exit_code, 1, "{}", up.stdout);
    let error = &up.json()["error"];
    assert_eq!(error["code"], "unfunded");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(&address), "{message}");
    assert!(message.contains("0.0001 ETH"), "{message}");
    assert!(message.contains("1000000 base units"), "{message}");
    assert!(message.contains("toon wallet fund"), "{message}");
    assert!(!machine.agent_node_home().join("control.sock").exists());
    assert_eq!(machine.toon(&["status", "--json"]).exit_code, 1);
}

#[test]
fn the_unfunded_refusal_on_mainnet_offers_no_funding_command() {
    let chain = FakeChain::start_unfunded();
    let machine = Machine::new();
    let init = machine.init_with(&["--network", "mainnet", "--evm-rpc-url", &chain.rpc_url()]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let up = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(up.json()["error"]["code"], "unfunded");
    let message = up.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("fund them yourself"), "{message}");
    assert!(!message.contains("wallet fund"), "{message}");
}

#[test]
fn wallet_fund_asks_the_faucet_for_the_addresses_and_then_up_starts() {
    let chain = FakeChain::start_unfunded();
    let faucet = FakeFaucet::funding(chain.funded());
    let machine = Machine::new();
    let init = machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--faucet-url",
        faucet.url(),
    ]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let address = init.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .unwrap()
        .to_owned();

    let fund = machine.toon(&["wallet", "fund", "--json"]);

    assert_eq!(fund.exit_code, 0, "{}", fund.stdout);
    assert_eq!(
        faucet.asked(),
        vec![("/api/base-sepolia/request".to_owned(), address)]
    );
    assert_eq!(fund.json()["lacking"].as_array().unwrap().len(), 0);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
}

#[test]
fn wallet_fund_says_what_the_faucet_left_unfunded() {
    let chain = FakeChain::start_unfunded();
    // A faucet that was never connected to the chain: it answers, and nothing arrives.
    let elsewhere = FakeChain::start_unfunded();
    let faucet = FakeFaucet::funding(elsewhere.funded());
    let machine = Machine::new();
    machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--faucet-url",
        faucet.url(),
    ]);

    let fund = machine.toon(&["wallet", "fund", "--json"]);

    assert_eq!(fund.exit_code, 0, "{}", fund.stdout);
    assert_eq!(fund.json()["lacking"].as_array().unwrap().len(), 2);
}

#[test]
fn wallet_fund_has_no_faucet_to_ask_on_mainnet_or_the_sandbox() {
    for network in ["mainnet", "sandbox"] {
        let machine = Machine::new();
        machine.init_with(&["--network", network]);

        let fund = machine.toon(&["wallet", "fund", "--json"]);

        assert_eq!(fund.exit_code, 1, "{network}: {}", fund.stdout);
        assert_eq!(fund.json()["error"]["code"], "faucet_unavailable");
    }
}

#[test]
fn wallet_fund_needs_an_agent_node() {
    let machine = Machine::new();

    let fund = machine.toon(&["wallet", "fund", "--json"]);

    assert_eq!(fund.exit_code, 3);
    assert_eq!(fund.json()["error"]["code"], "no_agent_node");
}

#[test]
fn init_refuses_more_decimals_than_a_token_amount_can_hold() {
    let machine = Machine::new();

    let init = machine.init_with(&["--evm-decimals", "19"]);

    assert_eq!(init.exit_code, 2, "{}", init.stdout);
    assert_eq!(init.json()["error"]["code"], "usage");
}

#[test]
fn up_refuses_when_a_settlement_key_cannot_be_read() {
    let chain = FakeChain::start_unfunded();
    let machine = Machine::new();
    machine.init_on(&chain);
    fs::remove_file(
        machine
            .agent_node_home()
            .join("connectors/0/settlement.key"),
    )
    .unwrap();

    let up = machine.toon(&["up", "--foreground", "--json"]);

    assert_eq!(up.json()["error"]["code"], "io", "{}", up.stdout);
    assert!(!machine.agent_node_home().join("control.sock").exists());
}

#[test]
fn wallet_fund_says_when_the_balances_cannot_be_read_after_asking_the_faucet() {
    let chain = FakeChain::start_unfunded();
    let faucet = FakeFaucet::funding(chain.funded());
    let machine = Machine::new();
    machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--faucet-url",
        faucet.url(),
    ]);
    drop(chain);

    let fund = machine.toon(&["wallet", "fund", "--json"]);

    assert_eq!(fund.exit_code, 0, "{}", fund.stdout);
    assert_eq!(faucet.asked().len(), 1);
    assert!(fund.json()["lacking"].is_null(), "{}", fund.stdout);
}
