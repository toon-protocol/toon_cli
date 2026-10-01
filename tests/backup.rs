mod support;

use std::fs;

use serde_json::Value;
use support::fake_chain::FakeChain;
use support::{Machine, PASSPHRASE};

fn passphrase(machine: &Machine, args: &[&str]) -> support::Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

fn wallet(machine: &Machine) -> Value {
    let run = passphrase(machine, &["wallet", "show", "--json"]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    run.json()["wallet"].clone()
}

fn endpoint(report: &Value) -> String {
    report["toon_apps"][0]["onion_endpoint"]
        .as_str()
        .expect("an onion endpoint")
        .to_owned()
}

#[test]
fn a_backup_restores_the_wallet_and_the_onion_endpoints_on_another_machine() {
    let chain = FakeChain::start();
    let before = Machine::new();
    let init = before.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let endpoint_before = endpoint(&init.json());
    let file = before.home().join("agent-node.backup");

    let backup = passphrase(
        &before,
        &[
            "wallet",
            "backup",
            "--json",
            "--out",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(backup.exit_code, 0, "{}", backup.stdout);
    assert_eq!(
        backup.json()["onion_endpoints"][0]["onion_endpoint"],
        endpoint_before
    );
    // Sealed: neither the mnemonic nor the key is readable in the file.
    let sealed = fs::read_to_string(&file).unwrap();
    let mnemonic = init.json()["mnemonic"].as_str().unwrap().to_owned();
    assert!(!sealed.contains(&mnemonic));
    let onion_key = fs::read(before.agent_node_home().join("connectors/0/onion.key")).unwrap();
    assert!(!sealed.contains(&hex_of(&onion_key)));

    // A file that exists is not overwritten.
    let again = passphrase(
        &before,
        &[
            "wallet",
            "backup",
            "--json",
            "--out",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(again.exit_code, 1);
    assert_eq!(again.json()["error"]["code"], "io");

    let after = Machine::new();
    let restore = passphrase(
        &after,
        &["wallet", "restore", "--json", file.to_str().unwrap()],
    );
    assert_eq!(restore.exit_code, 0, "{}", restore.stdout);
    assert_eq!(
        restore.json()["onion_endpoints"][0]["onion_endpoint"],
        endpoint_before
    );
    assert_eq!(wallet(&after), wallet(&before));

    let init = after.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(endpoint(&init.json()), endpoint_before);
    let config =
        fs::read_to_string(after.agent_node_home().join("connectors/0/connector.toml")).unwrap();
    assert!(config.contains(&endpoint_before), "{config}");
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn a_restore_into_a_home_with_a_wallet_changes_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let file = machine.home().join("b");
    let backup = passphrase(
        &machine,
        &["wallet", "backup", "--out", file.to_str().unwrap()],
    );
    assert_eq!(backup.exit_code, 0, "{}", backup.stdout);

    let restore = passphrase(
        &machine,
        &["wallet", "restore", "--json", file.to_str().unwrap()],
    );

    assert_eq!(restore.exit_code, 1);
    assert_eq!(restore.json()["error"]["code"], "io");
}

#[test]
fn a_backup_opens_only_with_its_passphrase_and_only_if_it_is_one() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let file = machine.home().join("b");
    assert_eq!(
        passphrase(
            &machine,
            &["wallet", "backup", "--out", file.to_str().unwrap()]
        )
        .exit_code,
        0
    );
    let other = Machine::new();

    let wrong = other.toon_with(
        &["wallet", "restore", "--json", file.to_str().unwrap()],
        |command| {
            command.env("TOON_PASSPHRASE", "another passphrase");
        },
    );
    assert_eq!(wrong.exit_code, 1);
    assert_eq!(wrong.json()["error"]["code"], "passphrase_wrong");

    let junk = other.home().join("junk");
    fs::write(&junk, "not a backup").unwrap();
    let corrupt = passphrase(
        &other,
        &["wallet", "restore", "--json", junk.to_str().unwrap()],
    );
    assert_eq!(corrupt.json()["error"]["code"], "keystore_corrupt");
    assert!(!other.agent_node_home().join("keystore.json").exists());
}

#[test]
fn init_restores_from_a_mnemonic_alone_with_new_onion_endpoints() {
    let chain = FakeChain::start();
    let before = Machine::new();
    let created = before.init_on(&chain);
    assert_eq!(created.exit_code, 0, "{}", created.stdout);
    let created = created.json();
    let mnemonic = created["mnemonic"].as_str().unwrap().to_owned();

    let after = Machine::new();
    let restored = after.toon_with(
        &[
            "init",
            "--json",
            "--accept-anyone-terms",
            "--from-mnemonic",
            "--evm-rpc-url",
            &chain.rpc_url(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            command.env("TOON_MNEMONIC", &mnemonic);
        },
    );

    assert_eq!(restored.exit_code, 0, "{}", restored.stdout);
    let restored = restored.json();
    assert_eq!(restored["restored"], true);
    assert_eq!(restored["onion_endpoints_changed"], true);
    assert!(restored["mnemonic"].is_null());
    assert_ne!(endpoint(&restored), endpoint(&created));
    assert_eq!(wallet(&after), wallet(&before));

    let text = after.toon_with(
        &["init", "--accept-anyone-terms", "--from-mnemonic"],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            command.env("TOON_MNEMONIC", &mnemonic);
        },
    );
    assert_eq!(text.exit_code, 1, "a wallet exists: {}", text.stdout);

    let fresh = Machine::new();
    let text = fresh.toon_with(
        &[
            "init",
            "--accept-anyone-terms",
            "--from-mnemonic",
            "--evm-rpc-url",
            &chain.rpc_url(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            command.env("TOON_MNEMONIC", &mnemonic);
        },
    );
    assert_eq!(text.exit_code, 0, "{}", text.stderr);
    assert!(
        text.stdout.contains("onion endpoints are new"),
        "{}",
        text.stdout
    );
}

#[test]
fn restoring_a_mnemonic_needs_one_and_not_from_a_flag() {
    let machine = Machine::new();
    let run = passphrase(
        &machine,
        &["init", "--json", "--accept-anyone-terms", "--from-mnemonic"],
    );
    assert_eq!(run.exit_code, 2);
    assert_eq!(run.json()["error"]["code"], "usage");
    assert!(!machine.agent_node_home().join("keystore.json").exists());
}

#[test]
fn a_restore_into_a_home_with_an_agent_node_but_no_wallet_changes_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let file = machine.home().join("b");
    let backup = passphrase(
        &machine,
        &["wallet", "backup", "--out", file.to_str().unwrap()],
    );
    assert_eq!(backup.exit_code, 0, "{}", backup.stdout);
    let onion_key = machine.agent_node_home().join("connectors/0/onion.key");
    let key_before = fs::read(&onion_key).unwrap();
    fs::remove_file(machine.agent_node_home().join("keystore.json")).unwrap();

    let restore = passphrase(
        &machine,
        &["wallet", "restore", "--json", file.to_str().unwrap()],
    );

    assert_eq!(restore.exit_code, 1, "{}", restore.stdout);
    assert_eq!(restore.json()["error"]["code"], "io");
    assert!(!machine.agent_node_home().join("keystore.json").exists());
    assert_eq!(fs::read(&onion_key).unwrap(), key_before);
}
