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
    let run = with_passphrase(machine, &["init", "--json", "--accept-anyone-terms"]);
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

    let run = with_passphrase(&machine, &["init", "--accept-anyone-terms"]);
    assert_eq!(run.exit_code, 0);
    let mnemonic = run
        .stdout
        .lines()
        .find(|line| line.split_whitespace().count() == 12)
        .expect("the mnemonic on a line of its own")
        .trim()
        .to_owned();

    for args in [
        &["init", "--json", "--accept-anyone-terms"][..],
        &["init", "--accept-anyone-terms"],
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

    let run = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
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
        let run = machine.toon(&["init", "--accept-anyone-terms", flag, PASSPHRASE, "--json"]);
        assert_eq!(run.exit_code, 2);
        assert_eq!(run.json()["error"]["code"], "usage");
    }
    assert_eq!(keystore_files(&machine), Vec::<String>::new());
}

#[test]
fn init_without_a_passphrase_fails_and_creates_nothing() {
    let machine = Machine::new();

    let run = machine.toon(&["init", "--json", "--accept-anyone-terms"]);

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_missing");
    assert_eq!(run.stderr, "");
    assert_eq!(keystore_files(&machine), Vec::<String>::new());
}

#[test]
fn an_empty_passphrase_is_refused() {
    let machine = Machine::new();

    let run = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
        command.env("TOON_PASSPHRASE", "");
    });

    assert_eq!(run.exit_code, 1);
    assert_eq!(run.json()["error"]["code"], "passphrase_missing");
}

#[test]
fn a_passphrase_file_that_is_not_there_is_named() {
    let machine = Machine::new();

    let run = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
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
    let again = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
        command.env("TOON_PASSPHRASE", "another passphrase");
    });
    let bare = machine.toon(&["init", "--json", "--accept-anyone-terms"]);

    for run in [&again, &bare] {
        assert_eq!(run.exit_code, 0, "{}", run.stdout);
        assert_eq!(run.json()["created"], false);
        assert!(run.json().get("mnemonic").is_none());
    }
    assert_eq!(fs::read(&keystore).unwrap(), before);
    // The agent node's home holds other files now; there is still one keystore, and no
    // half-written one beside it.
    let mut keystores = keystore_files(&machine);
    keystores.retain(|name| name.starts_with("keystore"));
    assert_eq!(keystores, vec!["keystore.json".to_owned()]);
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

/// Foundry's and Hardhat's default mnemonic, which `toon-client`'s tests use too.
const ANVIL: &str = "test test test test test test test test test test test junk";

/// Put a keystore holding `mnemonic` where `toon` looks for one, as `toon init` would
/// write it but with a cheap scrypt, so a test can open a wallet of a known mnemonic.
fn keystore_of(machine: &Machine, mnemonic: &str) {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    let (log_n, salt, nonce) = (1u8, [7u8; 16], [9u8; 12]);
    let mut key = [0u8; 32];
    let params = scrypt::Params::new(log_n, 8, 1, 32).unwrap();
    scrypt::scrypt(PASSPHRASE.as_bytes(), &salt, &params, &mut key).unwrap();
    let ciphertext = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key))
        .encrypt(&Nonce::from(nonce), mnemonic.as_bytes())
        .unwrap();
    let document = serde_json::json!({
        "version": 1,
        "kdf": { "name": "scrypt", "log_n": log_n, "r": 8, "p": 1, "salt": hex::encode(salt) },
        "cipher": { "name": "aes-256-gcm", "nonce": hex::encode(nonce) },
        "ciphertext": hex::encode(ciphertext),
    });
    fs::create_dir_all(machine.agent_node_home()).unwrap();
    fs::write(
        machine.agent_node_home().join("keystore.json"),
        document.to_string(),
    )
    .unwrap();
}

#[test]
fn wallet_show_gives_the_settlement_addresses_toon_client_gives() {
    let machine = Machine::new();
    keystore_of(&machine, ANVIL);

    let run = with_passphrase(&machine, &["wallet", "show", "--json"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let wallet = &run.json()["wallet"];
    // `toon-client`'s own vectors (`KeyDerivation.test.ts`) for account index 0.
    assert_eq!(
        wallet["chains"]["evm"][0]["address"],
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
    );
    assert_eq!(
        wallet["chains"]["solana"][0]["address"],
        "oeYf6KAJkLYhBuR8CiGc6L4D4Xtfepr85fuDgA9kq96"
    );
    // The wallet's own keys, as pinned in `src/derive.rs`.
    assert_eq!(
        wallet["agent_identity"],
        "ff535e30a4f7288a270c465f6d8c033b177342d5c5162bda8c4c3ed8e6b3267b"
    );
    assert_eq!(
        wallet["operator_write_key"],
        "ab202b62ab312a6026db3c651308c445af43983e89ee591f8bf51f7c6aa0756f"
    );
    assert_eq!(
        wallet["connector_identities"][0]["public_key"],
        "41c5dadd3b76286c4f4c4b0869b2d05e1c1a61bba8b75b7b884d2b1e1ee04079"
    );
}

#[test]
fn the_address_segment_is_the_start_of_the_connector_identity_key_and_follows_the_mnemonic() {
    let (first, second, restored) = (Machine::new(), Machine::new(), Machine::new());
    init(&first);
    init(&second);
    keystore_of(&restored, ANVIL);
    init(&restored);

    let segment_of = |machine: &Machine| {
        let shown = with_passphrase(machine, &["wallet", "show", "--json"]).json();
        let key = shown["wallet"]["connector_identities"][0]["public_key"]
            .as_str()
            .expect("an identity key")
            .to_owned();
        let segment = machine.segment_of("relay");
        assert_eq!(segment.len(), 16);
        assert_eq!(segment, key[..16], "the segment is the key's first 16 hex");
        let state = machine.toon(&["status", "--json"]).json();
        assert_eq!(
            state["agent_node"]["toon_apps"][0]["ilp_address"],
            format!("g.toon.{segment}").as_str()
        );
        let config = fs::read_to_string(
            machine
                .agent_node_home()
                .join("connectors/0/connector.toml"),
        )
        .unwrap();
        assert!(config.contains(&format!("addresses = [\"g.toon.{segment}\"]")));
        assert!(config.contains(&format!("prefix = \"g.toon.{segment}.relay\"")));
        assert!(!config.contains("\"g.toon.relay"), "{config}");
        segment
    };
    let (a, b, c) = (
        segment_of(&first),
        segment_of(&second),
        segment_of(&restored),
    );
    assert_ne!(a, b);

    // The same mnemonic gives the same addresses.
    let again = Machine::new();
    keystore_of(&again, ANVIL);
    init(&again);
    assert_eq!(segment_of(&again), c);
}
