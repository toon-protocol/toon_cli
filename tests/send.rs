mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

/// An agent node, running, with the fake relay behind its connector.
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
    up.report();
    Running {
        machine,
        _up: up,
        _chain: chain,
    }
}

#[test]
fn a_packet_to_the_operators_own_route_is_fulfilled() {
    let node = running();

    let relay = node.machine.relay_prefix();
    let run = node
        .machine
        .toon(&["send", &relay, "--amount", "0", "--yes", "--json"]);

    // The packet carries no event, so the relay it is delivered to refuses the write.
    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["status"], 400);
    assert_eq!(report["response"]["body"], support::NOT_A_WRITE);
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
}

#[test]
fn a_packet_to_a_route_under_the_prefix_is_fulfilled() {
    let node = running();

    let write = format!("{}.write", node.machine.relay_prefix());
    let run = node
        .machine
        .toon(&["send", &write, "--amount", "0", "--yes"]);

    assert_eq!(
        run.stdout,
        format!("Fulfilled: 0 base units to {write}. The app answered 400.\n")
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn a_rejected_packet_says_why_and_exits_non_zero() {
    let node = running();

    let run = node
        .machine
        .toon(&["send", "g.nobody.here", "--amount", "0", "--yes", "--json"]);

    let report = run.json();
    assert_eq!(report["outcome"], "rejected", "{report}");
    assert_eq!(report["reject"]["code"], "F02");
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn a_rejected_packet_is_readable_text_without_json() {
    let node = running();

    let run = node
        .machine
        .toon(&["send", "g.nobody.here", "--amount", "0", "--yes"]);

    assert!(
        run.stdout
            .starts_with("Rejected with F02: 0 base units to g.nobody.here."),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 1);
}

#[test]
fn send_needs_an_amount() {
    let machine = Machine::new();

    let run = machine.toon(&["send", "g.toon.abc.relay", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn send_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["send", "g.toon.relay", "--amount", "0", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
    assert_eq!(run.stderr, "");
}

#[test]
fn send_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["send", "g.toon.relay", "--amount", "0", "--yes", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn the_connector_lists_the_wallets_operator_write_key() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let key = fs::read(machine.agent_node_home().join("operator.key")).expect("the write key");
    let allowed = fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/operator-write-keys"),
    )
    .expect("the connector's allowlist");
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });

    assert_eq!(key.len(), 32);
    assert_eq!(
        allowed.trim(),
        shown.json()["wallet"]["operator_write_key"],
        "the connector lists the key the wallet shows"
    );
}

#[test]
fn an_agent_node_from_before_the_operator_write_key_still_comes_up() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let home = machine.agent_node_home();
    fs::remove_file(home.join("operator.key")).expect("forget the write key");
    fs::remove_file(home.join("connectors/0/operator-write-keys")).expect("and its allowlist");
    fs::remove_file(home.join("connectors/0/operator-bearer-token")).expect("and the token");

    let up = machine.start(&["up", "--foreground", "--json"]);

    let report = up.report();
    assert!(report.get("error").is_none(), "{report}");
}

#[test]
fn route_list_shows_the_routing_table() {
    let node = running();

    let run = node.machine.toon(&["route", "list", "--json"]);

    let routes = run.json()["routes"].clone();
    let status = node.machine.toon(&["status", "--json"]).json();
    let relay = status["agent_node"]["toon_apps"][0]["apps"][0]["address"]
        .as_str()
        .expect("the relay's address")
        .to_owned();
    assert_eq!(routes.as_array().map(Vec::len), Some(2), "{routes}");
    assert_eq!(routes[0]["prefix"], node.machine.relay_prefix());
    assert_eq!(routes[0]["handler_url"], format!("http://{relay}/write"));
    assert_eq!(routes[1]["prefix"], node.machine.ephemeral_prefix());
    assert_eq!(
        routes[1]["handler_url"],
        format!("http://{relay}/write-ephemeral")
    );
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.stderr, "");
}

#[test]
fn route_list_is_readable_text_without_json() {
    let node = running();

    let run = node.machine.toon(&["route", "list"]);

    assert!(
        run.stdout.starts_with(&format!(
            "{} -> http://127.0.0.1:",
            node.machine.relay_prefix()
        )),
        "{}",
        run.stdout
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn route_list_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&["route", "list", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}

/// An app at a URL that records each request it receives, head and body, and answers 201.
fn app() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/inbox", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&request).into_owned();
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let wanted = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    body.len() >= wanted
                });
                if read == 0 || complete {
                    break;
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request).into_owned());
            let _ = stream.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\nConnection: close\r\n\r\nmade!",
            );
        }
    });
    (url, seen)
}

fn add_app(node: &Running, url: &str) {
    let run = node.machine.toon(&[
        "add",
        "inbox",
        "--to",
        "relay",
        "--url",
        url,
        "--address",
        "g.toon.inbox",
        "--yes",
        "--json",
    ]);
    assert_eq!(run.exit_code, 0, "{}", run.stdout);
}

#[test]
fn a_request_with_a_method_a_path_and_a_body_reaches_the_app_and_its_answer_comes_back() {
    let (url, seen) = app();
    let node = running();
    add_app(&node, &url);
    let body = node.machine.home().join("body.json");
    fs::write(&body, br#"{"text":"hello"}"#).unwrap();

    let run = node.machine.toon(&[
        "send",
        "g.toon.inbox",
        "--amount",
        "0",
        "--method",
        "PUT",
        "--path",
        "/some/path?x=1",
        "--body",
        body.to_str().unwrap(),
        "--yes",
        "--json",
    ]);

    let report = run.json();
    assert_eq!(report["outcome"], "fulfilled", "{report}");
    assert_eq!(report["response"]["status"], 201);
    assert_eq!(report["response"]["body"], "made!");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let request = seen[0].to_ascii_lowercase();
    assert!(
        request.starts_with("put /inbox/some/path?x=1 "),
        "{request}"
    );
    assert!(
        request.contains("content-type: application/json"),
        "{request}"
    );
    assert!(request.ends_with(r#"{"text":"hello"}"#), "{request}");
}

#[test]
fn a_send_with_none_of_the_request_flags_is_an_empty_post_to_the_root() {
    let (url, seen) = app();
    let node = running();
    add_app(&node, &url);

    let run = node
        .machine
        .toon(&["send", "g.toon.inbox", "--amount", "0", "--yes"]);

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("POST /inbox "), "{seen:?}");
}

#[test]
fn an_unreadable_body_file_fails_before_sending_and_counts_for_nothing() {
    let (url, seen) = app();
    let node = running();
    add_app(&node, &url);
    let missing = node.machine.home().join("missing.json");

    let run = node.machine.toon(&[
        "send",
        "g.toon.inbox",
        "--amount",
        "0",
        "--body",
        missing.to_str().unwrap(),
        "--yes",
        "--json",
    ]);

    assert_ne!(run.exit_code, 0);
    assert!(run.stdout.contains("could not be read") || run.stderr.contains("could not be read"));
    assert!(seen.lock().unwrap().is_empty());
}
