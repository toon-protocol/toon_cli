mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::Value;
use support::{Machine, Run};

const PASSPHRASE: &str = "correct horse battery staple";

fn with_passphrase(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

fn init(machine: &Machine) -> Value {
    let run = with_passphrase(machine, &["init", "--json"]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    run.json()
}

fn mnemonic_of(created: &Value) -> String {
    created["mnemonic"].as_str().expect("a mnemonic").to_owned()
}

fn keystore_files(machine: &Machine) -> Vec<String> {
    fs::read_dir(machine.agent_node_home())
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn init_creates_an_encrypted_keystore_and_prints_the_mnemonic_once() {
    let machine = Machine::new();

    let created = init(&machine);

    assert_eq!(created["created"], true);
    let mnemonic = mnemonic_of(&created);
    assert_eq!(mnemonic.split(' ').count(), 12);
    let keystore_path = machine.agent_node_home().join("keystore.json");
    let keystore = fs::read_to_string(&keystore_path).expect("a keystore");
    assert!(
        !keystore.contains(&mnemonic),
        "the mnemonic is in the clear"
    );
    assert!(!keystore.contains(PASSPHRASE));
    let mode = fs::metadata(&keystore_path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the keystore is readable by others");
}

#[test]
fn init_in_text_shows_the_mnemonic_and_no_later_command_does() {
    let machine = Machine::new();

    let run = with_passphrase(&machine, &["init"]);
    assert_eq!(run.exit_code, 0);
    let mnemonic = run
        .stdout
        .lines()
        .find(|line| line.split_whitespace().count() == 12)
        .expect("the mnemonic on a line of its own")
        .trim()
        .to_owned();

    for args in [
        &["init", "--json"][..],
        &["init"],
        &["wallet", "show", "--json"],
        &["wallet", "show"],
        &["status", "--json"],
    ] {
        let later = with_passphrase(&machine, args);
        assert!(
            !later.stdout.contains(&mnemonic) && !later.stderr.contains(&mnemonic),
            "`toon {}` printed the mnemonic",
            args.join(" ")
        );
    }
}

#[test]
fn the_passphrase_can_come_from_a_file() {
    let machine = Machine::new();
    let file = machine.home().join("passphrase");
    fs::write(&file, format!("{PASSPHRASE}\n")).unwrap();

    let run = machine.toon_with(&["init", "--json"], |command| {
        command.env("TOON_PASSPHRASE_FILE", &file);
    });
    assert_eq!(run.exit_code, 0, "{}", run.stdout);

    // The newline the file ends with is not part of the passphrase.
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]);
    assert_eq!(shown.exit_code, 0, "{}", shown.stdout);
}

#[test]
fn the_passphrase_is_not_accepted_from_a_flag() {
    let machine = Machine::new();

    for flag in ["--passphrase", "--password"] {
        let run = machine.toon(&["init", flag, PASSPHRASE, "--json"]);
        assert_eq!(run.exit_code, 2);
        assert_eq!(run.json()["error"]["code"], "usage");
    }
    assert_eq!(keystore_files(&machine), Vec::<String>::new());
}

#[test]
fn init_without_a_passphrase_fails_and_creates_nothing() {
    let machine = Machine::new();

    let run = machine.toon(&["init", "--json"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_missing");
    assert_eq!(run.stderr, "");
    assert_eq!(keystore_files(&machine), Vec::<String>::new());
}

#[test]
fn an_empty_passphrase_is_refused() {
    let machine = Machine::new();

    let run = machine.toon_with(&["init", "--json"], |command| {
        command.env("TOON_PASSPHRASE", "");
    });

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_missing");
}

#[test]
fn a_passphrase_file_that_is_not_there_is_named() {
    let machine = Machine::new();

    let run = machine.toon_with(&["init", "--json"], |command| {
        command.env("TOON_PASSPHRASE_FILE", "/nonexistent/passphrase");
    });

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_unreadable");
}

#[test]
fn init_again_does_not_create_a_second_wallet() {
    let machine = Machine::new();
    init(&machine);
    let keystore = machine.agent_node_home().join("keystore.json");
    let before = fs::read(&keystore).unwrap();

    // Even with a different passphrase, and even with none.
    let again = machine.toon_with(&["init", "--json"], |command| {
        command.env("TOON_PASSPHRASE", "another passphrase");
    });
    let bare = machine.toon(&["init", "--json"]);

    for run in [&again, &bare] {
        assert_eq!(run.exit_code, 0, "{}", run.stdout);
        assert_eq!(run.json()["created"], false);
        assert!(run.json().get("mnemonic").is_none());
    }
    assert_eq!(fs::read(&keystore).unwrap(), before);
    assert_eq!(keystore_files(&machine), vec!["keystore.json".to_owned()]);
    let shown = with_passphrase(&machine, &["wallet", "show", "--json"]);
    assert_eq!(shown.exit_code, 0);
}

#[test]
fn wallet_show_lists_addresses_by_chain() {
    let machine = Machine::new();
    let created = init(&machine);

    let run = with_passphrase(&machine, &["wallet", "show", "--json"]);

    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
    let shown = run.json();
    assert_eq!(shown["wallet"], created["wallet"]);
    let wallet = &shown["wallet"];
    let evm = wallet["chains"]["evm"][0]["address"].as_str().unwrap();
    assert!(evm.starts_with("0x") && evm.len() == 42, "{evm}");
    assert_eq!(wallet["chains"]["evm"][0]["connector"], 0);
    assert!(
        wallet["chains"]["solana"][0]["address"]
            .as_str()
            .unwrap()
            .len()
            >= 32
    );
    assert_eq!(wallet["agent_identity"].as_str().unwrap().len(), 64);
    assert_eq!(wallet["operator_write_key"].as_str().unwrap().len(), 64);
    assert_eq!(wallet["connector_identities"][0]["connector"], 0);
}

#[test]
fn a_new_wallet_has_new_keys() {
    let (a, b) = (Machine::new(), Machine::new());

    let (a, b) = (init(&a), init(&b));

    assert_ne!(mnemonic_of(&a), mnemonic_of(&b));
    assert_ne!(a["wallet"], b["wallet"]);
}

#[test]
fn the_agent_identity_is_not_a_settlement_or_connector_key() {
    let machine = Machine::new();
    let wallet = init(&machine)["wallet"].clone();

    let identity = wallet["agent_identity"].as_str().unwrap();
    assert_eq!(
        wallet.to_string().matches(identity).count(),
        1,
        "the agent identity is also another key of the wallet: {wallet}"
    );
    assert_ne!(identity, wallet["operator_write_key"].as_str().unwrap());
    assert_ne!(
        identity,
        wallet["connector_identities"][0]["public_key"]
            .as_str()
            .unwrap()
    );
}

#[test]
fn wallet_show_without_a_wallet_exits_3() {
    let machine = Machine::new();

    let run = with_passphrase(&machine, &["wallet", "show", "--json"]);

    assert_eq!(run.exit_code, 3);
    assert_eq!(run.json()["error"]["code"], "no_wallet");
}

#[test]
fn wallet_show_with_the_wrong_passphrase_fails() {
    let machine = Machine::new();
    init(&machine);

    let run = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", "not it");
    });

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_wrong");
}

#[test]
fn wallet_show_without_a_passphrase_fails() {
    let machine = Machine::new();
    init(&machine);

    let run = machine.toon(&["wallet", "show", "--json"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_missing");
}

#[test]
fn a_damaged_keystore_is_reported_not_guessed_at() {
    let machine = Machine::new();
    init(&machine);
    fs::write(machine.agent_node_home().join("keystore.json"), "{}").unwrap();

    let run = with_passphrase(&machine, &["wallet", "show", "--json"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "keystore_corrupt");
}
