mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::fake_directory::{public_key, signed, FakeDirectory};
use support::fake_provider::FakeProvider;
use support::{Foreground, Machine, Run, PASSPHRASE};

/// What the peering's channel is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

/// What the Listing charges for a Lease Interval.
const PRICE: u64 = 1000;

const DIGEST: &str = "sha256:c02a7c7b9cac8e709f8cc42dc9e7aa8fdc36e35acfa19da7cd7dff2b8bd94fdd";

const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGxvbmdlbm91Z2hmb3JhdGVzdGtleQ tenant@test";

/// The provider's key: 32 bytes of this.
const PROVIDER: u8 = 0x22;

fn image() -> String {
    format!("docker.io/library/alpine@{DIGEST}")
}

fn listing_events(
    ilp_address: &str,
    connector_url: &str,
    seal_key: &str,
    directory: &FakeDirectory,
) -> String {
    let profile = json!({
        "ilp_address": ilp_address,
        "connector_url": connector_url,
        "connector_seal_key": seal_key,
        "relays": [directory.url()],
        "hidden": false,
        "liveness_cadence_s": 60,
    });
    directory.publish(signed(
        PROVIDER,
        1_700_000_000,
        10432,
        json!([["L", "toon.network"]]),
        &profile.to_string(),
    ));
    let listing = json!({
        "version": 1,
        "resources": { "cpu_millicores": 500, "memory_mb": 256, "storage_gb": 4 },
        "arch": "amd64",
        "lease_interval_s": 3600,
        "price": PRICE,
        "capabilities": [],
    });
    directory.publish(signed(
        PROVIDER,
        1_700_000_000,
        30432,
        json!([["d", "basic"], ["L", "toon.network"]]),
        &listing.to_string(),
    ));
    format!("30432:{}:basic", public_key(PROVIDER))
}

fn spawn_args<'a>(coordinate: &'a str, relay: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "compute",
        "spawn",
        coordinate,
        "--relay",
        relay,
        "--ssh-key",
        SSH_KEY,
        "--json",
    ];
    args.extend_from_slice(extra);
    args
}

fn lease_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
}

fn read_lease(dir: &Path) -> Value {
    let files = lease_files(dir);
    assert_eq!(files.len(), 1, "{files:?}");
    serde_json::from_slice(&fs::read(&files[0]).unwrap()).unwrap()
}

fn lease_file_json(workload: &str, state: &str, expires_at: Option<u64>) -> Value {
    json!({
        "version": 1,
        "workload_id": workload,
        "root_secret": "11".repeat(32),
        "provider": public_key(PROVIDER),
        "ilp_address": "g.nowhere",
        "connector_url": "http://127.0.0.1:1/ilp",
        "connector_seal_key": format!("0x{}", "04".repeat(65)),
        "listing": format!("30432:{}:basic", public_key(PROVIDER)),
        "listing_name": "basic",
        "listing_version": 1,
        "role": "standalone",
        "expires_at": expires_at,
        "paid": 1000,
        "lease_limit": 5000,
        "state": state,
        "error": null,
        "created_at": 1_700_000_000,
    })
}

fn write_lease(dir: &Path, lease: &Value) {
    fs::create_dir_all(dir).unwrap();
    let file = dir.join(format!("{}.json", lease["workload_id"].as_str().unwrap()));
    fs::write(&file, lease.to_string()).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
}

/// What `toon limit show` says is left of today's spending limit.
fn remaining_today(node: &Node) -> u128 {
    node.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .as_str()
        .expect("what remains today")
        .parse()
        .expect("a number")
}

fn error_code(run: &Run) -> String {
    run.json()["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no error code in {}{}", run.stdout, run.stderr))
        .to_owned()
}

// ---- without a node: what is decided before anything is paid, and the lease files ----

#[test]
fn a_tag_only_image_is_refused_locally() {
    let machine = Machine::new();
    let coordinate = format!("30432:{}:basic", public_key(PROVIDER));
    for tagged in [
        "alpine:3.20",
        "docker.io/library/alpine",
        "alpine:3@sha256:abc",
    ] {
        let run = machine.toon(&spawn_args(
            &coordinate,
            "ws://127.0.0.1:1",
            &["--image", tagged, "--yes"],
        ));
        assert_eq!(run.exit_code, 1, "{}", run.stdout);
        assert_eq!(error_code(&run), "image_not_pinned", "{tagged}");
        assert!(run.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Pin it by digest"));
    }
    assert!(lease_files(&machine.home().join(".toon/agent-node/leases")).is_empty());
}

#[test]
fn a_listing_that_is_missing_or_withdrawn_is_not_for_sale() {
    let machine = Machine::new();
    let directory = FakeDirectory::start();
    let image = image();
    let coordinate = listing_events(
        "g.nowhere",
        "http://127.0.0.1:1/ilp",
        &"04".repeat(65),
        &directory,
    );

    let missing = machine.toon(&spawn_args(
        &format!("30432:{}:other", public_key(PROVIDER)),
        &directory.url(),
        &["--image", &image, "--yes"],
    ));
    assert_eq!(
        error_code(&missing),
        "listing_not_found",
        "{}",
        missing.stdout
    );

    // The author deletes the Listing, as NIP-09 says.
    directory.publish(signed(
        PROVIDER,
        1_700_000_100,
        5,
        json!([["a", coordinate]]),
        "withdrawn",
    ));
    let withdrawn = machine.toon(&spawn_args(
        &coordinate,
        &directory.url(),
        &["--image", &image, "--yes"],
    ));
    assert_eq!(
        error_code(&withdrawn),
        "listing_withdrawn",
        "{}",
        withdrawn.stdout
    );
}

/// A connector's identity endpoint, answering with `key`.
fn identity(key: &str) -> String {
    let body = json!({ "publicKey": key }).to_string();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}")
}

#[test]
fn a_connector_that_reports_another_seal_key_than_the_profile_is_refused() {
    let machine = Machine::new();
    let directory = FakeDirectory::start();
    let connector = identity(&format!("04{}", "cd".repeat(64)));
    let coordinate = listing_events(
        "g.nowhere",
        &connector,
        &format!("04{}", "ab".repeat(64)),
        &directory,
    );
    let image = image();
    let run = machine.toon(&spawn_args(
        &coordinate,
        &directory.url(),
        &["--image", &image, "--yes"],
    ));
    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(error_code(&run), "seal_key_mismatch", "{}", run.stdout);
    assert!(lease_files(&machine.home().join(".toon/agent-node/leases")).is_empty());
}

#[test]
fn the_lease_files_are_listed_without_their_secret_and_forgotten_with_a_warning() {
    let machine = Machine::new();
    let dir = machine.home().join(".toon/agent-node/leases");
    let live = "aa".repeat(32);
    let failed = "bb".repeat(32);
    write_lease(&dir, &lease_file_json(&live, "live", Some(4_000_000_000)));
    let mut refused = lease_file_json(&failed, "failed", None);
    refused["error"] = json!({ "code": "refused_image", "message": "denied" });
    write_lease(&dir, &refused);

    let list = machine.toon(&["compute", "list", "--json"]);
    assert_eq!(list.exit_code, 0, "{}", list.stdout);
    assert!(!list.stdout.contains(&"11".repeat(32)), "{}", list.stdout);
    let leases = list.json()["leases"].clone();
    assert_eq!(leases[0]["workload_id"], live.as_str());
    assert_eq!(leases[0]["state"], "live");
    assert_eq!(leases[0]["listing_version"], 1);
    assert_eq!(leases[0]["paid"], 1000);
    assert_eq!(leases[0]["lease_limit"], 5000);
    assert_eq!(leases[1]["state"], "failed");
    assert_eq!(leases[1]["error"]["code"], "refused_image");
    let text = machine.toon(&["compute", "list"]);
    assert!(text.stdout.contains(&live) && text.stdout.contains("failed"));
    assert!(!text.stdout.contains(&"11".repeat(32)));

    let forgotten = machine.toon(&["compute", "forget", &live, "--json"]);
    assert_eq!(forgotten.exit_code, 0, "{}", forgotten.stdout);
    assert!(forgotten.json()["warning"]
        .as_str()
        .unwrap()
        .contains("may still be live"));
    assert!(!dir.join(format!("{live}.json")).exists());
    // A failed lease is nothing that is still live.
    let quiet = machine.toon(&["compute", "forget", &failed, "--json"]);
    assert!(quiet.json()["warning"].is_null(), "{}", quiet.stdout);

    let unknown = machine.toon(&["compute", "forget", &live, "--json"]);
    assert_eq!(error_code(&unknown), "lease_not_held");
    let status = machine.toon(&["compute", "status", &live, "--json"]);
    assert_eq!(error_code(&status), "lease_not_held");
}

#[test]
fn help_states_what_a_lease_costs_and_what_losing_its_file_loses() {
    let machine = Machine::new();
    let help = machine.toon(&["compute", "--help"]);
    assert_eq!(help.exit_code, 0);
    let flat = help.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("paid per Lease Interval and nothing is refunded"),
        "{flat}"
    );
    assert!(
        flat.contains("lose the file and you lose control of the lease"),
        "{flat}"
    );
    for args in [
        &["compute", "extend", "--help"],
        &["compute", "spawn", "--help"],
    ] {
        let help = machine.toon(args);
        assert_eq!(help.exit_code, 0);
        let flat = help.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("no `--until-done`"), "{flat}");
        assert!(flat.contains("extends by calling `extend`"), "{flat}");
    }
}

#[test]
fn a_lease_file_that_states_no_price_is_not_extended() {
    let machine = Machine::new();
    let dir = machine.home().join(".toon/agent-node/leases");
    let workload = "cc".repeat(32);
    // A file from before extends that paid nothing shows no price to pay again.
    let mut lease = lease_file_json(&workload, "live", Some(4_000_000_000));
    lease["paid"] = json!(0);
    write_lease(&dir, &lease);
    let run = machine.toon(&["compute", "extend", &workload, "--yes", "--json"]);
    assert_eq!(error_code(&run), "usage", "{}", run.stdout);
    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(message.contains("states no price"), "{message}");
}

#[test]
fn a_backup_and_a_restore_carry_the_lease_files() {
    let chain = FakeChain::start();
    let before = Machine::new();
    let init = before.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let workload = "cc".repeat(32);
    let lease = lease_file_json(&workload, "live", Some(4_000_000_000));
    write_lease(&before.agent_node_home().join("leases"), &lease);
    let file = before.home().join("agent-node.backup");
    let with_passphrase = |machine: &Machine, args: &[&str]| {
        machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    };

    let backup = with_passphrase(
        &before,
        &[
            "wallet",
            "backup",
            "--json",
            "--out",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(backup.exit_code, 0, "{}{}", backup.stdout, backup.stderr);
    assert_eq!(backup.json()["leases"], 1);
    // Sealed with the rest: the root secret is not readable in the file.
    assert!(!fs::read_to_string(&file)
        .unwrap()
        .contains(&"11".repeat(32)));

    let after = Machine::new();
    let restore = with_passphrase(
        &after,
        &["wallet", "restore", "--json", file.to_str().unwrap()],
    );
    assert_eq!(restore.exit_code, 0, "{}{}", restore.stdout, restore.stderr);
    let restored = after
        .agent_node_home()
        .join("leases")
        .join(format!("{workload}.json"));
    let restored_lease: Value = serde_json::from_slice(&fs::read(&restored).unwrap()).unwrap();
    assert_eq!(restored_lease, lease);
    assert_eq!(
        fs::metadata(&restored).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

// ---- with two nodes and a provider behind the far one ----

struct Node {
    machine: Machine,
    _up: Foreground,
    address: String,
}

impl Node {
    fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }

    fn toon(&self, args: &[&str]) -> Run {
        self.machine.toon_with(args, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }
}

fn node_on(chain: &AnvilChain) -> Node {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let report = up.report();
    let address = report["connector"]["address"]
        .as_str()
        .unwrap_or_else(|| panic!("the connector's address: {report}"))
        .to_owned();
    Node {
        machine,
        _up: up,
        address,
    }
}

struct World {
    near: Node,
    _far: Node,
    provider: FakeProvider,
    directory: FakeDirectory,
    coordinate: String,
}

/// A provider behind a node of its own, a Listing and Profile on a relay, and a near node
/// that peers with the far one.
fn world(chain: &AnvilChain) -> World {
    let near = node_on(chain);
    let far = node_on(chain);
    let provider = FakeProvider::start(&public_key(PROVIDER));
    let status = far.machine.toon(&["status", "--json"]).json();
    let toon_app = status["agent_node"]["toon_apps"][0]["name"]
        .as_str()
        .expect("a TOON app")
        .to_owned();
    let ilp = format!("g.toon.{}.compute", far.machine.segment("relay"));
    for (name, route, path, price) in [
        ("availability", "availability", "/availability", 0),
        ("status", "status", "/status", 0),
        ("terminate", "terminate", "/terminate", 0),
        ("spawn", "basic.v1.spawn", "/listings/basic/v1/spawn", PRICE),
        (
            "extend",
            "basic.v1.extend",
            "/listings/basic/v1/extend",
            PRICE,
        ),
    ] {
        let added = far.toon(&[
            "add",
            &format!("compute-{name}"),
            "--to",
            &toon_app,
            "--url",
            &provider.url(path),
            "--address",
            &format!("{ilp}.{route}"),
            "--price",
            &price.to_string(),
            "--yes",
        ]);
        assert_eq!(added.exit_code, 0, "{}{}", added.stdout, added.stderr);
    }
    let peered = near.toon(&[
        "peer",
        "add",
        &far.url(),
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--id",
        "far",
    ]);
    assert_eq!(peered.exit_code, 0, "{}{}", peered.stdout, peered.stderr);
    let routed = near.toon(&["route", "add", &ilp, "--peer", "far"]);
    assert_eq!(routed.exit_code, 0, "{}{}", routed.stdout, routed.stderr);
    let directory = FakeDirectory::start();
    let coordinate = listing_events(&ilp, &far.url(), &support::seal_key(&far.url()), &directory);
    World {
        near,
        _far: far,
        provider,
        directory,
        coordinate,
    }
}

impl World {
    fn leases(&self) -> PathBuf {
        self.near.machine.agent_node_home().join("leases")
    }

    fn spawn(&self, extra: &[&str]) -> Run {
        let relay = self.directory.url();
        let image = image();
        let mut args = spawn_args(&self.coordinate, &relay, &["--image", &image]);
        args.extend_from_slice(extra);
        self.near.toon(&args)
    }
}

#[test]
fn a_lease_is_rented_listed_read_and_terminated() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    world
        .provider
        .with(|state| state.lease_dir = Some(world.leases()));

    // Without `--yes` nothing is paid or written.
    let unconfirmed = world.spawn(&["--port", "443", "--env", "GREETING=hello"]);
    assert_eq!(
        error_code(&unconfirmed),
        "not_confirmed",
        "{}",
        unconfirmed.stdout
    );
    assert_eq!(world.provider.count("spawn"), 0);
    assert!(lease_files(&world.leases()).is_empty());

    // A provider that says the spawn would not run is believed, for free.
    world
        .provider
        .with(|state| state.refuse_availability = Some("refused_image".into()));
    let refused = world.spawn(&["--yes"]);
    assert_eq!(error_code(&refused), "would_not_run", "{}", refused.stdout);
    assert!(refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("refused_image"));
    assert_eq!(world.provider.count("spawn"), 0);
    assert!(lease_files(&world.leases()).is_empty());
    world
        .provider
        .with(|state| state.refuse_availability = None);

    let remaining_before = remaining_today(&world.near);
    let spawned = world.spawn(&[
        "--port",
        "443",
        "--env",
        "GREETING=hello",
        "--volume-gb",
        "2",
        "--max",
        "5000",
        "--yes",
    ]);
    assert_eq!(spawned.exit_code, 0, "{}{}", spawned.stdout, spawned.stderr);
    let report = spawned.json();
    let workload = report["workload_id"].as_str().unwrap().to_owned();
    assert_eq!(report["paid"], PRICE);
    assert_eq!(report["access"]["host"], "203.0.113.7");
    assert_eq!(report["access"]["ssh_port"], 40000);
    assert!(report["expires_at"].as_u64().unwrap() > 1_700_000_000);
    assert!(!spawned.stdout.contains(&"11".repeat(32)));

    // The file was there, mode 0600, when the packet arrived.
    assert_eq!(
        world.provider.with(|state| state.file_at_spawn),
        Some((true, 0o600))
    );
    let lease = read_lease(&world.leases());
    assert_eq!(lease["workload_id"], workload.as_str());
    assert_eq!(lease["state"], "live");
    assert_eq!(lease["paid"], PRICE);
    assert_eq!(lease["lease_limit"], 5000);
    assert_eq!(lease["expires_at"], report["expires_at"]);
    assert_eq!(lease["listing_version"], 1);
    assert_eq!(lease["provider"], public_key(PROVIDER));
    assert_eq!(lease["root_secret"].as_str().unwrap().len(), 64);
    assert!(!spawned
        .stdout
        .contains(lease["root_secret"].as_str().unwrap()));

    // What the provider read is the spec's request.
    let body = world.provider.last("spawn").expect("a spawn");
    let request = &body["request"];
    assert_eq!(body.as_object().unwrap().len(), 1);
    assert_eq!(request["op"], "spawn");
    assert_eq!(request["provider"], public_key(PROVIDER));
    assert_eq!(request["content"]["workload_id"], workload.as_str());
    assert_eq!(
        request["content"]["image"],
        json!({ "reference": "docker.io/library/alpine", "digest": DIGEST })
    );
    assert_eq!(request["content"]["env"], json!({ "GREETING": "hello" }));
    assert_eq!(
        request["content"]["ports"],
        json!([{ "container_port": 443, "protocol": "tcp" }])
    );
    assert_eq!(request["content"]["ssh_public_key"], SSH_KEY);
    assert_eq!(request["content"]["volume_gb"], 2);
    assert_eq!(
        world.provider.last("availability").unwrap()["image"],
        request["content"]["image"]
    );

    // The money moved: one Lease Interval, through the spending limit.
    assert_eq!(
        remaining_today(&world.near) + u128::from(PRICE),
        remaining_before
    );

    let list = world.near.toon(&["compute", "list", "--json"]);
    assert_eq!(list.json()["leases"][0]["workload_id"], workload.as_str());
    assert_eq!(list.json()["leases"][0]["state"], "live");
    assert!(!list.stdout.contains(lease["root_secret"].as_str().unwrap()));

    let status = world.near.toon(&["compute", "status", &workload, "--json"]);
    assert_eq!(status.exit_code, 0, "{}{}", status.stdout, status.stderr);
    assert_eq!(status.json()["state"], "running");
    assert_eq!(status.json()["expires_at"], report["expires_at"]);
    // The same token as the spawn's, which the provider stored.
    assert_eq!(
        world.provider.last("status").unwrap()["request"]["continuation"],
        request["continuation"]
    );

    let checked = world.near.toon(&["compute", "list", "--check", "--json"]);
    assert_eq!(checked.json()["leases"][0]["check"]["state"], "running");

    // Terminating says what it loses, and does nothing, until `--yes`.
    let careful = world
        .near
        .toon(&["compute", "terminate", &workload, "--json"]);
    assert_eq!(error_code(&careful), "not_confirmed", "{}", careful.stdout);
    assert!(careful.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nothing is refunded"));
    assert_eq!(world.provider.count("terminate"), 0);
    let terminated = world
        .near
        .toon(&["compute", "terminate", &workload, "--yes", "--json"]);
    assert_eq!(
        terminated.exit_code, 0,
        "{}{}",
        terminated.stdout, terminated.stderr
    );
    assert_eq!(
        terminated.json()["state"],
        json!({ "ended": "termination" })
    );
    assert_eq!(read_lease(&world.leases())["state"], "ended");
    let again = world.near.toon(&["compute", "status", &workload, "--json"]);
    assert_eq!(error_code(&again), "lease_refused");
}

#[test]
fn a_spawn_over_a_limit_is_refused_with_the_total_and_the_limit_and_sends_nothing() {
    let chain = AnvilChain::start();
    let world = world(&chain);

    // The lease limit, `--max`.
    let over_max = world.spawn(&["--max", "999", "--yes"]);
    assert_eq!(over_max.exit_code, 1, "{}", over_max.stdout);
    assert_eq!(error_code(&over_max), "lease_limit");
    let message = over_max.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("1000") && message.contains("999"),
        "{message}"
    );

    // The spending limit, `toon limit`.
    let set = world
        .near
        .toon(&["limit", "set", "--max-per-command", "999", "--json"]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);
    let over_limit = world.spawn(&["--yes"]);
    assert_eq!(
        error_code(&over_limit),
        "spending_limit",
        "{}",
        over_limit.stdout
    );
    let message = over_limit.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("1000") && message.contains("999"),
        "{message}"
    );

    // Not even the free `availability` question.
    assert_eq!(world.provider.count("spawn"), 0);
    assert_eq!(world.provider.count("availability"), 0);
    assert!(lease_files(&world.leases()).is_empty());

    // The agent key's limit, read from the connector.
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("agent.json");
    let created = world.near.toon(&[
        "agent-key",
        "create",
        "--max-per-command",
        "999",
        "--out",
        key_file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(created.exit_code, 0, "{}{}", created.stdout, created.stderr);
    let nowhere = tempfile::tempdir().unwrap();
    let as_agent = |args: &[&str]| -> Run {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let run = world.near.machine.toon_with(args, |command| {
                command
                    .env("HOME", nowhere.path())
                    .env("TOON_AGENT_KEY", &key_file);
            });
            if run.exit_code != 1
                || !run.stdout.contains("did not answer")
                || Instant::now() > deadline
            {
                return run;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };
    let relay = world.directory.url();
    let image = image();
    let agent_spawn = spawn_args(&world.coordinate, &relay, &["--image", &image, "--yes"]);
    let refused = as_agent(&agent_spawn);
    assert_eq!(refused.exit_code, 1, "{}{}", refused.stdout, refused.stderr);
    assert_eq!(error_code(&refused), "spending_limit", "{}", refused.stdout);
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("1000") && message.contains("999"),
        "{message}"
    );
    assert_eq!(world.provider.count("spawn"), 0);
    assert_eq!(world.provider.count("availability"), 0);
    assert!(!keys.path().join("leases").exists());

    // Raised, the same agent rents, and its lease file sits beside its key file.
    let public = created.json()["agent_key"]["public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let raised = world.near.toon(&[
        "agent-key",
        "set",
        &public,
        "--max-per-command",
        "5000",
        "--json",
    ]);
    assert_eq!(raised.exit_code, 0, "{}{}", raised.stdout, raised.stderr);
    // `--max-per-command` of the wallet stays 999: the agent key pays through its own limit.
    let rented = as_agent(&agent_spawn);
    assert_eq!(rented.exit_code, 0, "{}{}", rented.stdout, rented.stderr);
    let lease = read_lease(&keys.path().join("leases"));
    assert_eq!(lease["state"], "live");
    assert_eq!(world.provider.count("spawn"), 1);
    let workload = lease["workload_id"].as_str().unwrap().to_owned();
    let status = as_agent(&["compute", "status", &workload, "--json"]);
    assert_eq!(status.exit_code, 0, "{}{}", status.stdout, status.stderr);
    assert_eq!(status.json()["state"], "running");
    let ended = as_agent(&["compute", "terminate", &workload, "--yes", "--json"]);
    assert_eq!(ended.exit_code, 0, "{}{}", ended.stdout, ended.stderr);
    let listed = as_agent(&["compute", "list", "--json"]);
    assert_eq!(listed.json()["leases"][0]["state"], "ended");
}

#[test]
fn a_spawn_the_provider_refuses_leaves_the_file_failed_with_what_was_paid() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    world
        .provider
        .with(|state| state.spawn_error = Some("no_capacity".into()));

    let refused = world.spawn(&["--no-check", "--yes"]);
    assert_eq!(refused.exit_code, 1, "{}{}", refused.stdout, refused.stderr);
    assert_eq!(error_code(&refused), "lease_refused");
    assert!(refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no_capacity"));
    assert_eq!(world.provider.count("availability"), 0);
    let lease = read_lease(&world.leases());
    assert_eq!(lease["state"], "failed");
    assert_eq!(lease["paid"], PRICE);
    assert_eq!(lease["error"]["code"], "no_capacity");
    let list = world.near.toon(&["compute", "list"]);
    assert!(list.stdout.contains("failed"), "{}", list.stdout);
}

#[test]
fn a_spawn_whose_file_cannot_be_written_is_not_sent() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    // A file where the directory of lease files should be.
    fs::write(world.leases(), "not a directory").unwrap();

    let refused = world.spawn(&["--no-check", "--yes"]);
    assert_eq!(refused.exit_code, 1, "{}{}", refused.stdout, refused.stderr);
    assert_eq!(world.provider.count("spawn"), 0);
    assert!(refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nothing was sent"));
}

fn extend_fixture() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/toon-network-fixtures/extend.ok.json");
    serde_json::from_str(&fs::read_to_string(path).expect("the extend fixture")).unwrap()
}

#[test]
fn intervals_are_prepaid_in_one_command_and_the_file_follows_the_last_answer() {
    let chain = AnvilChain::start();
    let world = world(&chain);

    // `--for` rounds up and says the count and the total before it pays.
    let rounded = world.spawn(&["--for", "150m", "--yes"]);
    assert_eq!(rounded.exit_code, 0, "{}{}", rounded.stdout, rounded.stderr);
    assert!(
        rounded.stderr.contains("3 Lease Intervals") && rounded.stderr.contains("3000"),
        "{}",
        rounded.stderr
    );
    assert_eq!(rounded.json()["intervals"], 3);
    assert_eq!(rounded.json()["paid"], 3 * PRICE);
    let unconfirmed = world.spawn(&["--for", "61m"]);
    assert_eq!(error_code(&unconfirmed), "not_confirmed");
    assert!(unconfirmed.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("2 Lease Intervals"));
    assert_eq!(world.provider.count("spawn"), 1);

    // One spawn and two extends; the extend body is the fixture's request body.
    assert_eq!(world.provider.count("extend"), 2);
    let workload = rounded.json()["workload_id"].as_str().unwrap().to_owned();
    let fixture = extend_fixture();
    let mut expected = fixture["request_body"].clone();
    expected["workload_id"] = json!(workload);
    assert_eq!(world.provider.last("extend"), Some(expected));
    let lease = read_lease(&world.leases());
    assert_eq!(lease["paid"], 3 * PRICE);
    assert_eq!(lease["expires_at"], rounded.json()["expires_at"]);
    assert_eq!(lease["expires_at"], world.provider.expires_at());

    // `extend` prepays more, and `--max` changes the lease limit.
    let unconfirmed = world.near.toon(&["compute", "extend", &workload, "--json"]);
    assert_eq!(error_code(&unconfirmed), "not_confirmed");
    assert_eq!(world.provider.count("extend"), 2);
    let remaining_before = remaining_today(&world.near);
    let extended = world.near.toon(&[
        "compute",
        "extend",
        &workload,
        "--intervals",
        "2",
        "--max",
        "9000",
        "--yes",
        "--json",
    ]);
    assert_eq!(
        extended.exit_code, 0,
        "{}{}",
        extended.stdout, extended.stderr
    );
    assert_eq!(world.provider.count("extend"), 4);
    assert_eq!(extended.json()["paid"], 5 * PRICE);
    let lease = read_lease(&world.leases());
    assert_eq!(lease["paid"], 5 * PRICE);
    assert_eq!(lease["lease_limit"], 9000);
    assert_eq!(lease["expires_at"], extended.json()["expires_at"]);
    assert_eq!(lease["expires_at"], world.provider.expires_at());
    assert_eq!(
        remaining_today(&world.near) + u128::from(2 * PRICE),
        remaining_before
    );

    // A lease this node does not hold is refused.
    let strange = world
        .near
        .toon(&["compute", "extend", &"ab".repeat(32), "--yes", "--json"]);
    assert_eq!(error_code(&strange), "lease_not_held");
}

#[test]
fn an_extend_over_a_limit_is_refused_with_the_total_and_the_limit_and_sends_nothing() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    let spawned = world.spawn(&["--max", "2500", "--yes"]);
    assert_eq!(spawned.exit_code, 0, "{}{}", spawned.stdout, spawned.stderr);
    let workload = spawned.json()["workload_id"].as_str().unwrap().to_owned();

    // The lease limit counts what the lease has paid already: 1000 + 2000 > 2500.
    let over_max = world.near.toon(&[
        "compute",
        "extend",
        &workload,
        "--intervals",
        "2",
        "--yes",
        "--json",
    ]);
    assert_eq!(error_code(&over_max), "lease_limit", "{}", over_max.stdout);
    let message = over_max.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("2000") && message.contains("2500"),
        "{message}"
    );

    // `toon limit`, per command: the whole extend is one command.
    let set = world
        .near
        .toon(&["limit", "set", "--max-per-command", "1500", "--json"]);
    assert_eq!(set.exit_code, 0, "{}{}", set.stdout, set.stderr);
    let over_limit = world.near.toon(&[
        "compute",
        "extend",
        &workload,
        "--intervals",
        "2",
        "--max",
        "9000",
        "--yes",
        "--json",
    ]);
    assert_eq!(
        error_code(&over_limit),
        "spending_limit",
        "{}",
        over_limit.stdout
    );
    let message = over_limit.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("2000") && message.contains("1500"),
        "{message}"
    );
    assert_eq!(world.provider.count("extend"), 0);
    // The refused `--max` was not kept.
    assert_eq!(read_lease(&world.leases())["lease_limit"], 2500);

    // The agent key's limits, read from the connector: per packet and per day.
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("agent.json");
    let created = world.near.toon(&[
        "agent-key",
        "create",
        "--max-per-command",
        "5000",
        "--out",
        key_file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(created.exit_code, 0, "{}{}", created.stdout, created.stderr);
    let public = created.json()["agent_key"]["public_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let nowhere = tempfile::tempdir().unwrap();
    let as_agent = |args: &[&str]| -> Run {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let run = world.near.machine.toon_with(args, |command| {
                command
                    .env("HOME", nowhere.path())
                    .env("TOON_AGENT_KEY", &key_file);
            });
            if run.exit_code != 1
                || !run.stdout.contains("did not answer")
                || Instant::now() > deadline
            {
                return run;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };
    let relay = world.directory.url();
    let image = image();
    let agent_spawn = spawn_args(&world.coordinate, &relay, &["--image", &image, "--yes"]);
    let rented = as_agent(&agent_spawn);
    assert_eq!(rented.exit_code, 0, "{}{}", rented.stdout, rented.stderr);
    let agent_workload = rented.json()["workload_id"].as_str().unwrap().to_owned();
    let per_packet = world.near.toon(&[
        "agent-key",
        "set",
        &public,
        "--max-per-command",
        "999",
        "--json",
    ]);
    assert_eq!(
        per_packet.exit_code, 0,
        "{}{}",
        per_packet.stdout, per_packet.stderr
    );
    let refused = as_agent(&["compute", "extend", &agent_workload, "--yes", "--json"]);
    assert_eq!(error_code(&refused), "spending_limit", "{}", refused.stdout);
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("1000") && message.contains("999"),
        "{message}"
    );
    assert_eq!(world.provider.count("extend"), 0);
    let per_day = world.near.toon(&[
        "agent-key",
        "set",
        &public,
        "--max-per-command",
        "2500",
        "--max-per-day",
        "2500",
        "--json",
    ]);
    assert_eq!(per_day.exit_code, 0, "{}{}", per_day.stdout, per_day.stderr);
    let refused = as_agent(&[
        "compute",
        "extend",
        &agent_workload,
        "--intervals",
        "3",
        "--yes",
        "--json",
    ]);
    assert_eq!(error_code(&refused), "spending_limit", "{}", refused.stdout);
    let message = refused.json()["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("3000") && message.contains("1500"),
        "{message}"
    );
    assert_eq!(world.provider.count("extend"), 0);
    // Within them, the agent extends.
    let extended = as_agent(&["compute", "extend", &agent_workload, "--yes", "--json"]);
    assert_eq!(
        extended.exit_code, 0,
        "{}{}",
        extended.stdout, extended.stderr
    );
    assert_eq!(world.provider.count("extend"), 1);
}

#[test]
fn a_provider_that_fails_the_second_extend_leaves_the_first_in_the_file() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    world.provider.with(|state| state.fail_extend_at = Some(2));
    let run = world.spawn(&["--intervals", "4", "--yes"]);
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(error_code(&run), "lease_partly_paid", "{}", run.stdout);
    // The spawn and the first extend, then the failed one: nothing after it, no retry.
    assert_eq!(world.provider.count("spawn"), 1);
    assert_eq!(world.provider.count("extend"), 2);
    let lease = read_lease(&world.leases());
    assert_eq!(lease["state"], "live");
    // The first extend's answer, not the one of a later packet.
    let expires_at = lease["expires_at"].as_u64().unwrap();
    assert_eq!(expires_at, world.provider.expires_at());
    // The failed extend was billed all the same, as a refused spawn is.
    assert_eq!(lease["paid"], 3 * PRICE);
    let message = run.json()["error"]["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("2 of 4") && message.contains(&expires_at.to_string()),
        "{message}"
    );
    // The lease is live and paid for, so what reaches it is printed all the same.
    assert!(message.contains("ssh -p 40000 203.0.113.7"), "{message}");
}

const REPO: &str = "30617:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:toon";
const COMMIT: &str = "6666666666666666666666666666666666666666";

fn deployment_flags(environment: &str) -> [&str; 7] {
    [
        "--deployment",
        environment,
        "--repo",
        REPO,
        "--commit",
        COMMIT,
        "--yes",
    ]
}

#[test]
fn a_deployment_that_cannot_be_published_never_fails_the_lease_command() {
    let chain = AnvilChain::start();
    let world = world(&chain);

    // This node follows no relay to publish to, so the publish fails after the lease is bought.
    let flags = deployment_flags("production").to_vec();
    let spawned = world.spawn(&flags);
    assert_eq!(spawned.exit_code, 0, "{}{}", spawned.stdout, spawned.stderr);
    assert_eq!(world.provider.count("spawn"), 1);
    let deployment = &spawned.json()["deployment"];
    assert_eq!(deployment["published"], false, "{deployment}");
    let retry = deployment["retry"].as_str().unwrap().to_owned();
    let workload = spawned.json()["workload_id"].as_str().unwrap().to_owned();
    let expires_at = spawned.json()["expires_at"].as_u64().unwrap();
    for part in [
        "toon deployment publish production",
        &format!("--workload {workload}"),
        &format!("--expires-at {expires_at}"),
        &format!("--repo {REPO}"),
    ] {
        assert!(retry.contains(part), "{retry}");
    }
    assert!(
        spawned.stderr.contains("warning") && spawned.stderr.contains(&retry),
        "{}",
        spawned.stderr
    );
    let lease = read_lease(&world.leases());
    assert_eq!(lease["state"], "live");
    assert_eq!(lease["deployment"]["environment"], "production");

    // An extend of a lease that records an environment tries again with the new expiry.
    let extended = world
        .near
        .toon(&["compute", "extend", &workload, "--yes", "--json"]);
    assert_eq!(
        extended.exit_code, 0,
        "{}{}",
        extended.stdout, extended.stderr
    );
    let new_expiry = extended.json()["expires_at"].as_u64().unwrap();
    assert!(new_expiry > expires_at);
    let deployment = &extended.json()["deployment"];
    assert_eq!(deployment["published"], false, "{deployment}");
    assert!(deployment["retry"]
        .as_str()
        .unwrap()
        .contains(&format!("--expires-at {new_expiry}")));

    // A lease without the flag carries no environment and touches no Deployment.
    let plain = world.spawn(&["--yes"]);
    assert_eq!(plain.exit_code, 0, "{}{}", plain.stdout, plain.stderr);
    assert!(plain.json().get("deployment").is_none());

    // Malformed inputs are refused before anything is paid.
    let before = world.provider.count("spawn");
    let mut bad = deployment_flags("production").to_vec();
    bad[5] = "abc";
    let refused = world.spawn(&bad);
    assert_eq!(
        error_code(&refused),
        "deployment_refused",
        "{}",
        refused.stdout
    );
    assert_eq!(world.provider.count("spawn"), before);
}

#[test]
fn an_agent_key_without_the_signing_scope_is_refused_before_any_packet_is_sent() {
    let chain = AnvilChain::start();
    let world = world(&chain);
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("agent.json");
    let created = world.near.toon(&[
        "agent-key",
        "create",
        "--scope",
        "send",
        "--out",
        key_file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(created.exit_code, 0, "{}{}", created.stdout, created.stderr);
    let nowhere = tempfile::tempdir().unwrap();
    let relay = world.directory.url();
    let image = image();
    let mut args = spawn_args(&world.coordinate, &relay, &["--image", &image]);
    args.extend_from_slice(&deployment_flags("production"));
    let deadline = Instant::now() + Duration::from_secs(30);
    let run = loop {
        let run = world.near.machine.toon_with(&args, |command| {
            command
                .env("HOME", nowhere.path())
                .env("TOON_AGENT_KEY", &key_file);
        });
        if !run.stdout.contains("did not answer") || Instant::now() > deadline {
            break run;
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("sign"), "{}", run.stdout);
    assert_eq!(world.provider.count("spawn"), 0);
    assert_eq!(world.provider.count("availability"), 0);
    assert!(!keys.path().join("leases").exists());
}
