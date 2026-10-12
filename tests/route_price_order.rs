mod support;

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run, PASSPHRASE};

/// An agent node that is up with a second TOON app beside the relay's, and a home of
/// nobody's for the agent to run in.
struct Node {
    machine: Machine,
    nowhere: tempfile::TempDir,
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
        nowhere: tempfile::tempdir().expect("a directory"),
        _chain: chain,
        _up: up,
    }
}

impl Node {
    fn operator(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    }

    /// Create an agent key and return its key file and public half.
    fn create(&self, extra: &[&str]) -> (PathBuf, String) {
        let mut args = vec!["agent-key", "create", "--json"];
        args.extend_from_slice(extra);
        let run = self.operator(&args);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        let json = run.json();
        (
            PathBuf::from(json["key_file"].as_str().expect("key file")),
            json["agent_key"]["public_key"].as_str().unwrap().to_owned(),
        )
    }

    /// Run `toon` as the agent does: no home, and the key file named in the environment.
    fn agent(&self, key: &Path, args: &[&str]) -> Run {
        // The supervisor binds its order address a moment after the key is made.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let run = self.machine.toon_with(args, |command| {
                command
                    .env("HOME", self.nowhere.path())
                    .env("TOON_AGENT_KEY", key);
            });
            if run.exit_code != 1
                || !run.stdout.contains("did not answer")
                || Instant::now() > deadline
            {
                return run;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// What the connector charges on the route to the relay.
    fn price(&self) -> u64 {
        let routes = self.machine.toon(&["route", "list", "--json"]).json();
        routes["routes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|route| route["prefix"] == self.machine.relay_prefix().as_str())
            .expect("the relay's route")["price"]
            .as_u64()
            .unwrap()
    }

    fn set_price(&self, key: &Path, price: &str) -> Run {
        let prefix = self.machine.relay_prefix();
        self.agent(key, &["route", "price", &prefix, price, "--yes", "--json"])
    }
}

#[test]
fn route_price_with_an_agent_key_changes_the_price_the_connector_charges() {
    let node = node();
    let (key, _) = node.create(&["--scope", "route-price"]);

    let run = node.set_price(&key, "7");

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["price"], 7);
    assert_eq!(run.json()["restarted"], true);
    assert_eq!(node.price(), 7);
}

#[test]
fn the_third_restart_of_a_key_limited_to_two_an_hour_is_refused_and_restarts_nothing() {
    let node = node();
    let (key, _) = node.create(&["--scope", "route-price", "--max-restarts", "2"]);

    assert_eq!(node.set_price(&key, "1").exit_code, 0);
    assert_eq!(node.set_price(&key, "2").exit_code, 0);
    let pid = |node: &Node| {
        let status = node.machine.toon(&["status", "--json"]).json();
        status["toon_apps"][0]["connector"]["pid"].clone()
    };
    let before = pid(&node);

    let refused = node.set_price(&key, "3");

    assert_eq!(refused.exit_code, 1, "{}{}", refused.stdout, refused.stderr);
    let error = refused.json()["error"].clone();
    assert_eq!(error["code"], "rate_limited", "{error}");
    assert!(
        error["message"].as_str().unwrap().contains("next restart"),
        "{error}"
    );
    assert_eq!(node.price(), 2);
    assert_eq!(pid(&node), before, "the connector was not restarted");
}

#[test]
fn agent_key_set_changes_the_restart_limit() {
    let node = node();
    let (key, public) = node.create(&["--scope", "route-price", "--max-restarts", "1"]);
    assert_eq!(node.set_price(&key, "1").exit_code, 0);
    let refused = node.set_price(&key, "2");
    assert_eq!(refused.json()["error"]["code"], "rate_limited");

    let set = node.operator(&["agent-key", "set", &public, "--max-restarts", "2", "--json"]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);
    assert_eq!(set.json()["agent_key"]["max_restarts"], 2);

    let run = node.set_price(&key, "2");
    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(node.price(), 2);
}

#[test]
fn a_key_defaults_to_six_restarts_an_hour() {
    let node = node();
    let (_, public) = node.create(&["--scope", "route-price"]);
    let list = node.machine.toon(&["agent-key", "list", "--json"]).json();
    let key = list["agent_keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["public_key"] == public.as_str())
        .unwrap();
    assert_eq!(key["max_restarts"], 6);
}

#[test]
fn a_key_without_the_scope_and_a_key_not_granted_on_the_toon_app_are_refused() {
    let node = node();
    let (unscoped, _) = node.create(&["--scope", "status"]);
    let (elsewhere, _) = node.create(&["--scope", "route-price", "--toon-app", "second"]);
    let before = node.price();

    let run = node.set_price(&unscoped, "9");
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("out_of_scope"),
        "{}",
        run.stdout
    );

    let run = node.set_price(&elsewhere, "9");
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        run.json()["error"]["code"],
        "unknown_name",
        "{}",
        run.stdout
    );

    assert_eq!(node.price(), before);
}

#[test]
fn the_operator_with_the_home_is_never_limited() {
    let node = node();
    let (key, _) = node.create(&["--scope", "route-price", "--max-restarts", "1"]);
    assert_eq!(node.set_price(&key, "1").exit_code, 0);
    assert_eq!(
        node.set_price(&key, "2").json()["error"]["code"],
        "rate_limited"
    );

    let prefix = node.machine.relay_prefix();
    for price in ["3", "4", "5"] {
        let run = node
            .machine
            .toon(&["route", "price", &prefix, price, "--yes", "--json"]);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    }
    assert_eq!(node.price(), 5);
}
