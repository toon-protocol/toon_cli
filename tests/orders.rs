mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use connector_operator::signing::sign_request;
use serde_json::{json, Value};
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, PASSPHRASE};

/// An agent node that is up with a second TOON app, `second`, beside the relay's.
struct Node {
    machine: Machine,
    _chain: FakeChain,
    _up: Foreground,
}

fn node() -> Node {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let created = machine.toon_with(
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
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(created.exit_code, 0, "{}{}", created.stdout, created.stderr);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    Node {
        machine,
        _chain: chain,
        _up: up,
    }
}

/// An agent key as its key file holds it.
struct Agent {
    public: String,
    keypair: ed25519_dalek_v1::Keypair,
    supervisor: String,
}

impl Node {
    fn create(&self, extra: &[&str]) -> Agent {
        let mut args = vec!["agent-key", "create", "--json"];
        args.extend_from_slice(extra);
        let run = self.machine.toon_with(&args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        });
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        let file = PathBuf::from(run.json()["key_file"].as_str().expect("key file"));
        let key: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        let secret = ed25519_dalek_v1::SecretKey::from_bytes(
            &hex::decode(key["private_key"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let public = ed25519_dalek_v1::PublicKey::from(&secret);
        Agent {
            public: key["public_key"].as_str().unwrap().to_owned(),
            keypair: ed25519_dalek_v1::Keypair { secret, public },
            supervisor: key["supervisor"].as_str().unwrap().to_owned(),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Agent {
    /// The headers and body of an order signed by this key.
    fn signed(&self, path: &str, body: &str) -> (String, String, String) {
        let created = now();
        sign_request(
            &self.keypair,
            "POST",
            path,
            body.as_bytes(),
            created,
            Some(created + 60),
        )
    }

    fn send(&self, path: &str, body: &str) -> (u16, Value) {
        let (input, signature, digest) = self.signed(path, body);
        post(
            &self.supervisor,
            path,
            body,
            Some((input, signature, digest)),
        )
    }
}

fn post(
    supervisor: &str,
    path: &str,
    body: &str,
    signed: Option<(String, String, String)>,
) -> (u16, Value) {
    let client = reqwest::blocking::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut request = client
            .post(format!("{supervisor}{path}"))
            .header("content-type", "application/json")
            .body(body.to_owned());
        if let Some((input, signature, digest)) = &signed {
            request = request
                .header("signature-input", input)
                .header("signature", signature)
                .header("content-digest", digest);
        }
        match request.send() {
            Ok(response) => {
                let status = response.status().as_u16();
                return (status, response.json().unwrap_or(Value::Null));
            }
            // The listener binds once the record names an address, a moment after the key.
            Err(error) => {
                assert!(Instant::now() < deadline, "no supervisor listens: {error}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn names(reply: &Value) -> Vec<&str> {
    reply["toon_apps"]
        .as_array()
        .expect("toon apps")
        .iter()
        .map(|app| app["name"].as_str().unwrap())
        .collect()
}

#[test]
fn a_status_order_names_only_the_toon_apps_the_key_is_granted_on() {
    let node = node();
    let on_second = node.create(&["--scope", "status", "--toon-app", "second"]);
    let on_both = node.create(&[
        "--scope",
        "status",
        "--toon-app",
        "relay",
        "--toon-app",
        "second",
    ]);

    let (status, reply) = on_second.send("/orders/status", "");
    assert_eq!(status, 200, "{reply}");
    assert_eq!(names(&reply), ["second"]);
    let text = reply.to_string();
    assert!(
        !text.contains(node.machine.home().to_str().unwrap()),
        "{text}"
    );

    let (status, reply) = on_both.send("/orders/status", "");
    assert_eq!(status, 200, "{reply}");
    let mut both = names(&reply);
    both.sort_unstable();
    assert_eq!(both, ["relay", "second"]);
}

#[test]
fn a_logs_order_needs_the_logs_scope_and_the_answer_names_it() {
    let node = node();
    let status_only = node.create(&["--scope", "status"]);
    let logs = node.create(&["--scope", "logs"]);

    let (status, reply) = status_only.send("/orders/logs", "{}");
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"]["code"], "out_of_scope");
    assert_eq!(reply["error"]["scope"], "logs");
    assert!(reply["error"]["message"].as_str().unwrap().contains("logs"));

    let (status, reply) = logs.send("/orders/logs", &json!({ "lines": 5 }).to_string());
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["toon_app"], "relay");
    assert!(reply["lines"].is_array(), "{reply}");
    assert!(
        !reply
            .to_string()
            .contains(node.machine.home().to_str().unwrap()),
        "{reply}"
    );

    let (status, reply) = logs.send("/orders/logs", &json!({ "toon_app": "second" }).to_string());
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["error"]["code"], "unknown_name", "{reply}");
}

#[test]
fn an_unsigned_order_and_one_from_an_unknown_key_are_refused() {
    let node = node();
    let agent = node.create(&["--scope", "status"]);

    let (status, reply) = post(&agent.supervisor, "/orders/status", "", None);
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "unsigned");

    // A key the record does not hold, signing validly.
    let stranger_secret = ed25519_dalek_v1::SecretKey::from_bytes(&[7u8; 32]).unwrap();
    let stranger = Agent {
        public: String::new(),
        keypair: ed25519_dalek_v1::Keypair {
            public: (&stranger_secret).into(),
            secret: stranger_secret,
        },
        supervisor: agent.supervisor.clone(),
    };
    let (status, reply) = stranger.send("/orders/status", "");
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "unknown_key");

    // Signed for another body than the one sent.
    let (input, signature, digest) = agent.signed("/orders/status", "{}");
    let (status, reply) = post(
        &agent.supervisor,
        "/orders/status",
        "{\"toon_app\":\"relay\"}",
        Some((input, signature, digest)),
    );
    assert_eq!(status, 401, "{reply}");
}

#[test]
fn a_revoked_key_is_refused_on_its_next_order_without_a_restart() {
    let node = node();
    let agent = node.create(&["--scope", "status"]);
    let (status, reply) = agent.send("/orders/status", "");
    assert_eq!(status, 200, "{reply}");

    let revoked = node.machine.toon(&["agent-key", "revoke", &agent.public]);
    assert_eq!(revoked.exit_code, 0, "{}{}", revoked.stdout, revoked.stderr);

    let (status, reply) = agent.send("/orders/status", "");
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "unknown_key");
}

#[test]
fn an_expired_key_is_refused() {
    let node = node();
    let agent = node.create(&["--scope", "status", "--expires", "2099-01-01T00:00:00Z"]);
    let (status, reply) = agent.send("/orders/status", "");
    assert_eq!(status, 200, "{reply}");

    let set = node.machine.toon_with(
        &[
            "agent-key",
            "set",
            &agent.public,
            "--expires",
            "2001-01-01T00:00:00Z",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);

    let (status, reply) = agent.send("/orders/status", "");
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"]["code"], "key_expired");
}

#[test]
fn a_signature_that_has_expired_is_refused() {
    let node = node();
    let agent = node.create(&["--scope", "status"]);
    let created = now() - 120;
    let (input, signature, digest) = sign_request(
        &agent.keypair,
        "POST",
        "/orders/status",
        b"",
        created,
        Some(created + 60),
    );
    let (status, reply) = post(
        &agent.supervisor,
        "/orders/status",
        "",
        Some((input, signature, digest)),
    );
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "expired");
}

#[test]
fn the_same_signed_order_sent_twice_is_refused_the_second_time() {
    let node = node();
    let agent = node.create(&["--scope", "status"]);
    let signed = agent.signed("/orders/status", "");

    let (status, reply) = post(
        &agent.supervisor,
        "/orders/status",
        "",
        Some(signed.clone()),
    );
    assert_eq!(status, 200, "{reply}");
    let (status, reply) = post(&agent.supervisor, "/orders/status", "", Some(signed));
    assert_eq!(status, 401, "{reply}");
    assert_eq!(reply["error"]["code"], "replayed");
}

#[test]
fn orders_are_heard_on_loopback_only() {
    let node = node();
    let agent = node.create(&["--scope", "status"]);
    let address: SocketAddr = agent
        .supervisor
        .strip_prefix("http://")
        .expect("an http address")
        .parse()
        .expect("an address");
    assert!(address.ip().is_loopback(), "{address}");
    agent.send("/orders/status", "");

    // Nothing but loopback listens on the supervisor's port.
    let tcp = fs::read_to_string("/proc/net/tcp").unwrap_or_default();
    let port = format!(":{:04X} ", address.port());
    for line in tcp.lines().filter(|line| line.contains(&port)) {
        let local = line.split_whitespace().nth(1).unwrap_or_default();
        let listening = line.split_whitespace().nth(3) == Some("0A");
        if listening && local.ends_with(&port.trim_end().to_owned()) {
            assert!(
                local.starts_with("0100007F"),
                "listening beyond loopback: {line}"
            );
        }
    }
}
