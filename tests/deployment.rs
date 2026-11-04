mod support;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run};

const REPO: &str = "30617:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:toon";
const LISTING: &str =
    "30432:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:small";
const PRIMARY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const WORKLOAD: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const COMMIT: &str = "6666666666666666666666666666666666666666";
/// Year 2100: far from any clock the test runs on.
const FAR: &str = "4102444800";

fn publish(machine: &Machine, extra: &[&str]) -> Run {
    let mut args = vec![
        "deployment",
        "publish",
        "production",
        "--repo",
        REPO,
        "--commit",
        COMMIT,
        "--listing",
        LISTING,
        "--primary",
        PRIMARY,
        "--workload",
        WORKLOAD,
        "--expires-at",
        FAR,
        "--json",
    ];
    args.extend_from_slice(extra);
    machine.toon(&args)
}

fn refused(run: &Run, mention: &str) {
    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    let json = run.json();
    assert_eq!(json["error"]["code"], "deployment_refused", "{json}");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(message.contains(mention), "{message}");
    assert!(message.contains("Nothing was sent"), "{message}");
}

#[test]
fn the_help_says_a_deployment_is_optional_and_public() {
    let machine = Machine::new();
    for subcommand in ["--help", "publish --help"] {
        let mut args = vec!["deployment"];
        args.extend(subcommand.split(' '));
        let run = machine.toon(&args);
        assert_eq!(run.exit_code, 0);
        let help = run.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            help.contains("optional and public") || help.contains("optional, public"),
            "{help}"
        );
        assert!(help.contains("workload id"), "{help}");
    }
}

#[test]
fn every_malformed_input_is_refused_before_anything_is_sent() {
    let machine = Machine::new();
    refused(
        &publish(&machine, &["--expiration", "4102444801"]),
        "later than",
    );
    refused(&publish(&machine, &["--expiration", "5"]), "past");
    refused(&publish(&machine, &["--url", "http://x.example"]), "https");
    refused(&publish(&machine, &["--image", "abc"]), "--image");
    refused(&publish(&machine, &["--standby", PRIMARY]), "--standby");
    refused(
        &machine.toon(&[
            "deployment",
            "publish",
            "production",
            "--repo",
            LISTING,
            "--commit",
            COMMIT,
            "--listing",
            LISTING,
            "--primary",
            PRIMARY,
            "--workload",
            WORKLOAD,
            "--expires-at",
            FAR,
            "--json",
        ]),
        "--repo",
    );
    refused(
        &machine.toon(&[
            "deployment",
            "publish",
            "production",
            "--repo",
            REPO,
            "--commit",
            "abc",
            "--listing",
            LISTING,
            "--primary",
            PRIMARY,
            "--workload",
            WORKLOAD,
            "--expires-at",
            FAR,
            "--json",
        ]),
        "--commit",
    );
    refused(
        &machine.toon(&["deployment", "update", "production", "--json"]),
        "Name what changed",
    );
    refused(
        &machine.toon(&[
            "deployment",
            "update",
            "production",
            "--commit",
            "abc",
            "--json",
        ]),
        "--commit",
    );
}

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
    let _ = up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
    }
}

impl Running {
    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }
}

#[test]
fn update_and_delete_need_a_current_deployment_and_never_rebuild_one() {
    let node = running();
    let relay = ["--relay", "ws://127.0.0.1:9", "--json"];

    let mut update = vec!["deployment", "update", "production", "--commit", COMMIT];
    update.extend(relay);
    refused(&node.toon(&update), "no current Deployment");

    let mut delete = vec!["deployment", "delete", "production"];
    delete.extend(relay);
    refused(&node.toon(&delete), "no current Deployment");
}

#[test]
fn list_says_so_when_the_own_relay_holds_none() {
    let node = running();
    let list = node.toon(&["deployment", "list", "--json"]);
    assert_eq!(list.exit_code, 0, "{}", list.stdout);
    assert_eq!(list.json()["deployments"], serde_json::json!([]));
}

#[test]
fn a_deployment_on_the_own_relay_is_listed_with_its_time_left() {
    let node = running();
    let identity = node
        .toon(&["event", "sign", "--kind", "1", "--json"])
        .json()["event"]["pubkey"]
        .as_str()
        .unwrap()
        .to_owned();
    let _ = identity;
    let soon = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600)
        .to_string();
    let tags = serde_json::json!([
        ["d", "production"],
        ["a", REPO],
        ["c", COMMIT],
        ["a", LISTING],
        ["p", PRIMARY, "primary"],
        ["x", WORKLOAD, "workload"],
        ["expiration", soon],
        ["L", "toon.network"]
    ]);
    let published = node.toon(&[
        "event",
        "publish",
        "--kind",
        "30437",
        "--tags",
        &tags.to_string(),
        "--json",
    ]);
    assert_eq!(published.exit_code, 0, "{}", published.stdout);

    let list = node.toon(&["deployment", "list", "--json"]);
    assert_eq!(list.exit_code, 0, "{}", list.stdout);
    let deployments = list.json()["deployments"].as_array().unwrap().clone();
    assert_eq!(deployments.len(), 1, "{deployments:?}");
    assert_eq!(deployments[0]["environment"], "production");
    assert_eq!(deployments[0]["repository"], REPO);
    assert_eq!(deployments[0]["primary"], PRIMARY);
    assert_eq!(deployments[0]["expiring"], true);
}
