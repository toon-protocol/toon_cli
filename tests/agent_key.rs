mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

fn with_passphrase(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    })
}

struct Node {
    machine: Machine,
    _chain: FakeChain,
}

fn node() -> Node {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    Node {
        machine,
        _chain: chain,
    }
}

impl Node {
    fn create(&self, extra: &[&str]) -> (String, PathBuf, Run) {
        let mut args = vec!["agent-key", "create", "--json"];
        args.extend_from_slice(extra);
        let run = with_passphrase(&self.machine, &args);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        let json = run.json();
        let public = json["agent_key"]["public_key"]
            .as_str()
            .expect("key")
            .to_owned();
        let file = PathBuf::from(json["key_file"].as_str().expect("key file"));
        (public, file, run)
    }

    fn list_file(&self, connector: u32) -> String {
        fs::read_to_string(
            self.machine
                .agent_node_home()
                .join("connectors")
                .join(connector.to_string())
                .join("operator-controller-keys"),
        )
        .expect("the connector's list of agent keys")
    }
}

#[test]
fn create_writes_a_key_file_and_the_connectors_entry() {
    let node = node();
    let out = node.machine.home().join("agent.json");

    let (public, file, run) = node.create(&[
        "--scope",
        "send,status",
        "--max-per-command",
        "10",
        "--max-per-day",
        "100",
        "--expires",
        "2099-01-01T00:00:00Z",
        "--out",
        out.to_str().unwrap(),
    ]);

    assert_eq!(file, out);
    let key: serde_json::Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(key["private_key"].as_str().unwrap().len(), 64);
    assert_eq!(key["public_key"], public.as_str());
    assert!(key["supervisor"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));
    let relay = key["toon_apps"]["relay"]
        .as_str()
        .expect("the relay TOON app's surface");
    assert!(relay.starts_with("http://127.0.0.1:"), "{relay}");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!run.stdout.contains(key["private_key"].as_str().unwrap()));

    let list = node.list_file(0);
    assert!(list.contains(&format!("key = \"{public}\"")), "{list}");
    assert!(list.contains("scope = [\"packets\"]"), "{list}");
    assert!(
        list.contains("expires = \"2099-01-01T00:00:00Z\""),
        "{list}"
    );
    assert!(
        list.contains("spending_limit = { per_write = 10, per_day = 100 }"),
        "{list}"
    );
}

#[test]
fn a_key_with_no_scope_cannot_withdraw_from_or_close_a_channel() {
    let node = node();

    let (public, _, run) = node.create(&[]);

    let list = node.list_file(0);
    for write in ["packets", "peers", "routes-peers", "routes-leased"] {
        assert!(list.contains(&format!("\"{write}\"")), "{write}: {list}");
    }
    for write in [
        "channels-withdraw",
        "channels-land",
        "channels-open",
        "channels-fund",
    ] {
        assert!(!list.contains(write), "{write}: {list}");
    }
    let scope = run.json()["agent_key"]["scope"].clone();
    for order in [
        "status",
        "logs",
        "apps",
        "route-price",
        "sign",
        "read-messages",
    ] {
        assert!(
            scope.as_array().unwrap().iter().any(|name| name == order),
            "{order}"
        );
    }
    assert!(list.contains(&public));
}

#[test]
fn list_shows_the_public_half_and_never_a_private_key() {
    let node = node();
    let (public, file, _) = node.create(&[]);
    let private = serde_json::from_str::<serde_json::Value>(&fs::read_to_string(file).unwrap())
        .unwrap()["private_key"]
        .as_str()
        .unwrap()
        .to_owned();

    let text = node.machine.toon(&["agent-key", "list"]);
    let json = node.machine.toon(&["agent-key", "list", "--json"]);

    assert_eq!(text.exit_code, 0);
    assert!(text.stdout.contains(&public));
    assert!(json.json()["agent_keys"][0]["toon_apps"]
        .to_string()
        .contains("relay"));
    for output in [&text, &json] {
        assert!(!output.stdout.contains(&private));
        assert!(!output.stderr.contains(&private));
    }
    let record =
        fs::read_to_string(node.machine.agent_node_home().join("agent-keys.json")).unwrap();
    assert!(!record.contains(&private));
}

#[test]
fn create_and_set_need_the_passphrase() {
    let node = node();

    let created = node.machine.toon(&["agent-key", "create", "--json"]);
    assert_eq!(created.exit_code, 1, "{}", created.stdout);
    assert_eq!(created.json()["error"]["code"], "passphrase_missing");
    assert!(node
        .machine
        .toon(&["agent-key", "list"])
        .stdout
        .contains("No agent keys"));

    let (public, _, _) = node.create(&[]);
    let set = node
        .machine
        .toon(&["agent-key", "set", &public, "--max-per-day", "5", "--json"]);
    assert_eq!(set.exit_code, 1, "{}", set.stdout);
    assert_eq!(set.json()["error"]["code"], "passphrase_missing");
    assert!(!node.list_file(0).contains("per_day = 5 "));
}

#[test]
fn set_changes_the_record_and_the_connectors_entry() {
    let node = node();
    let (public, _, _) = node.create(&[]);

    let set = with_passphrase(
        &node.machine,
        &[
            "agent-key",
            "set",
            &public,
            "--scope",
            "routes",
            "--max-per-command",
            "7",
            "--max-per-day",
            "70",
            "--expires",
            "2099-06-01T00:00:00Z",
            "--json",
        ],
    );

    assert_eq!(set.exit_code, 0, "{}", set.stdout);
    let list = node.list_file(0);
    assert!(
        list.contains("scope = [\"routes-leased\", \"routes-peers\"]"),
        "{list}"
    );
    assert!(
        list.contains("expires = \"2099-06-01T00:00:00Z\""),
        "{list}"
    );
    assert!(!list.contains("spending_limit"), "{list}");
    let shown = node.machine.toon(&["agent-key", "list", "--json"]).json();
    assert_eq!(shown["agent_keys"][0]["spending_limit"]["per_day"], 70);
    assert_eq!(
        shown["agent_keys"][0]["scope"],
        serde_json::json!(["routes"])
    );
}

#[test]
fn a_key_is_on_the_list_of_the_toon_apps_it_is_granted_on_and_of_no_other() {
    let node = node();
    let created = with_passphrase(
        &node.machine,
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
    );
    assert_eq!(created.exit_code, 0, "{}", created.stdout);

    let (on_relay, _, _) = node.create(&[]);
    let (on_second, file, _) = node.create(&["--toon-app", "second"]);
    let (on_both, _, _) = node.create(&["--toon-app", "relay", "--toon-app", "second"]);

    let relay = node.list_file(0);
    let second = node.list_file(1);
    assert!(relay.contains(&on_relay) && relay.contains(&on_both) && !relay.contains(&on_second));
    assert!(
        second.contains(&on_second) && second.contains(&on_both) && !second.contains(&on_relay)
    );
    let key: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    assert!(key["toon_apps"].get("second").is_some() && key["toon_apps"].get("relay").is_none());

    let moved = with_passphrase(
        &node.machine,
        &["agent-key", "set", &on_relay, "--toon-app", "second"],
    );
    assert_eq!(moved.exit_code, 0, "{}{}", moved.stdout, moved.stderr);
    assert!(!node.list_file(0).contains(&on_relay));
    assert!(node.list_file(1).contains(&on_relay));

    let unknown = with_passphrase(
        &node.machine,
        &["agent-key", "create", "--toon-app", "nope", "--json"],
    );
    assert_eq!(unknown.exit_code, 1);
    assert_eq!(unknown.json()["error"]["code"], "unknown_name");
}

#[test]
fn revoke_needs_no_passphrase_and_a_running_connector_refuses_the_key_at_once() {
    let node = node();
    let up: Foreground = node.machine.start(&["up", "--foreground", "--json"]);
    let address = up.report()["connector"]["address"]
        .as_str()
        .unwrap()
        .to_owned();
    let (public, file, _) = node.create(&[]);
    let key: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let secret = ed25519_dalek_v1::SecretKey::from_bytes(
        &hex::decode(key["private_key"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let keypair = ed25519_dalek_v1::Keypair {
        public: (&secret).into(),
        secret,
    };
    let signed = |method: &str, path: &str| {
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (input, signature, digest) = connector_operator::signing::sign_request(
            &keypair,
            method,
            path,
            b"",
            created,
            Some(created + 60),
        );
        reqwest::blocking::Client::new()
            .request(method.parse().unwrap(), format!("http://{address}{path}"))
            .header("content-digest", digest)
            .header("signature-input", input)
            .header("signature", signature)
            .send()
            .expect("the connector answers")
            .status()
            .as_u16()
    };
    // A write in the key's scope: removing a peer route, here one there is none of.
    let write = || signed("DELETE", "/routes/peers/g.nothing");
    assert_eq!(signed("GET", "/write-keys"), 200, "a listed key reads");
    let allowed = write();
    assert!(
        allowed != 401 && allowed != 403,
        "a listed key may write: {allowed}"
    );

    let revoked = node
        .machine
        .toon(&["agent-key", "revoke", &public, "--json"]);

    assert_eq!(revoked.exit_code, 0, "{}", revoked.stdout);
    assert_eq!(
        write(),
        401,
        "the connector refuses the revoked key's next write without a restart"
    );
    assert_eq!(signed("GET", "/write-keys"), 401);
    assert!(!node.list_file(0).contains(&public));
    assert!(node
        .machine
        .toon(&["agent-key", "list"])
        .stdout
        .contains("No agent keys"));
    let again = node
        .machine
        .toon(&["agent-key", "revoke", &public, "--json"]);
    assert_eq!(again.exit_code, 1);
    assert_eq!(again.json()["error"]["code"], "unknown_name");
}

#[test]
fn the_connectors_own_operator_key_still_writes() {
    let node = node();
    let (_, _, _) = node.create(&[]);

    let list = fs::read_to_string(
        node.machine
            .agent_node_home()
            .join("connectors/0/operator-write-keys"),
    )
    .unwrap();

    assert_eq!(list.lines().count(), 1);
}
