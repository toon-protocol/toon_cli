mod support;

use support::fake_chain::{self, FakeChain};
use support::{Machine, Run, PASSPHRASE};

fn init_with_code(machine: &Machine, chain: &FakeChain, code: &[&str]) -> Run {
    let mut args = vec![
        "--evm-rpc-url",
        "",
        "--evm-token",
        fake_chain::TOKEN,
        "--evm-decimals",
        "6",
        "--evm-asset-name",
        "USDC",
        "--evm-asset-version",
        "2",
        "--evm-transfer-method",
        "permit2",
    ];
    let rpc_url = chain.rpc_url();
    args[1] = &rpc_url;
    args.extend_from_slice(code);
    machine.init_with(&args)
}

fn balances(machine: &Machine) -> serde_json::Value {
    machine
        .toon_with(&["wallet", "balances", "--json"], |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
        .json()
}

#[test]
fn a_custom_asset_code_is_kept_and_reported() {
    let chain = FakeChain::start();
    let machine = Machine::new();

    let init = init_with_code(&machine, &chain, &["--evm-asset-code", "FOO"]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(
        balances(&machine)["balances"][0]["token"]["asset_code"],
        "FOO"
    );
}

#[test]
fn a_custom_token_with_no_code_has_none() {
    let chain = FakeChain::start();
    let machine = Machine::new();

    assert_eq!(init_with_code(&machine, &chain, &[]).exit_code, 0);

    assert!(balances(&machine)["balances"][0]["token"]["asset_code"].is_null());
}

#[test]
fn a_malformed_asset_code_is_a_usage_error() {
    for code in ["usdc", "TOOLONGASSET1", "", "US-DC"] {
        let machine = Machine::new();

        let init = machine.init_with(&["--evm-asset-code", code]);

        assert_eq!(init.exit_code, 2, "{code}: {}", init.stdout);
    }
}

#[test]
fn a_state_from_before_the_asset_code_reads_with_its_profiles() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(init_with_code(&machine, &chain, &[]).exit_code, 0);
    let recorded = std::fs::read(machine.agent_node_home().join("state.json")).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&recorded).unwrap();
    state["toon_apps"][0]["evm"]
        .as_object_mut()
        .unwrap()
        .remove("asset_code");
    machine.write_agent_node_file("state.json", state.to_string());

    assert_eq!(
        balances(&machine)["balances"][0]["token"]["asset_code"],
        "USDC"
    );
}
