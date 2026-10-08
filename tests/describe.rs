//! `toon describe` prints a connector's self-description, free.

mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use support::fake_chain::FakeChain;
use support::{Foreground, Machine};

struct Running {
    machine: Machine,
    _up: Foreground,
    _chain: FakeChain,
    url: String,
}

fn running() -> Running {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let address = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .to_owned();
    Running {
        machine,
        _up: up,
        _chain: chain,
        url: format!("http://{address}/ilp"),
    }
}

/// A server that answers every request with `status` and `body`.
fn answering(status: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ilp", listener.local_addr().unwrap());
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    url
}

#[test]
fn the_self_description_of_a_connector_at_a_url_is_printed_unaltered() {
    let node = running();
    let direct: serde_json::Value = reqwest::blocking::get(&node.url)
        .expect("the connector answers")
        .json()
        .expect("a self-description");

    // From a machine that has no agent node of its own.
    let elsewhere = Machine::new();
    let run = elsewhere.toon(&["describe", &node.url, "--json"]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json(), serde_json::json!({ "description": direct }));
    assert!(direct["routes"].is_array(), "{direct}");
}

#[test]
fn without_a_url_the_agent_nodes_own_connector_is_described() {
    let node = running();
    let own: serde_json::Value = reqwest::blocking::get(&node.url).unwrap().json().unwrap();

    for args in [
        vec!["describe", "--json"],
        vec!["describe", "--app", "relay", "--json"],
    ] {
        let run = node.machine.toon(&args);
        assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
        assert_eq!(run.json()["description"], own);
    }
    let missing = node.machine.toon(&["describe", "--app", "nope", "--json"]);
    assert_ne!(missing.exit_code, 0);
}

#[test]
fn the_text_names_each_route_with_its_price() {
    let url = answering(
        "200 OK",
        r#"{"ilpAddress":"g.toon.x","batchSettlements":[{"chain":"evm:base:8453","token":"0xusdc"}],"routes":[{"prefix":"g.toon.x.relay","price":"7","pricePerKib":"3","request":{"method":"POST","params":{"text":"a string"},"path":"/echo"}},{"prefix":"g.toon.x.other","price":"9"}]}"#,
    );

    let run = Machine::new().toon(&["describe", &url]);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("ilpAddress: g.toon.x"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("batchSettlements:\n  - chain: evm:base:8453\n    token: 0xusdc\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "g.toon.x.relay  price 7, 3 per KiB, states a request\n    request: \
             {\"method\":\"POST\",\"params\":{\"text\":\"a string\"},\"path\":\"/echo\"}\n"
        ),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("g.toon.x.other  price 9, no request stated"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_url_that_does_not_give_a_self_description_fails() {
    let machine = Machine::new();
    let refused = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}/ilp", refused.local_addr().unwrap());
    drop(refused);
    for url in [
        closed,
        answering("404 Not Found", "{}"),
        answering("200 OK", "not json"),
        answering("200 OK", r#"{"name":"no routes"}"#),
    ] {
        let run = machine.toon(&["describe", &url, "--json"]);
        assert_eq!(run.exit_code, 1, "{url}: {}", run.stdout);
        assert_eq!(run.json()["error"]["code"], "describe_failed", "{url}");
    }
}

#[test]
fn a_hidden_agent_node_describes_another_host_through_the_overlay_or_not_at_all() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let run = machine.toon_with(
        &["describe", "http://connector.example/ilp", "--json"],
        |command| {
            command
                .env("TOON_PASSPHRASE", support::PASSPHRASE)
                .env("TOON_OVERLAY", "none");
        },
    );

    assert_eq!(run.exit_code, 1, "{}", run.stdout);
    assert_eq!(run.json()["error"]["code"], "overlay_unavailable");
}

#[test]
fn describing_pays_nothing_and_counts_against_no_limit() {
    let node = running();
    let before = node.machine.toon(&["limit", "show", "--json"]).stdout;

    let run = node.machine.toon(&["describe", &node.url, "--json"]);
    assert_eq!(run.exit_code, 0);

    assert_eq!(
        node.machine.toon(&["limit", "show", "--json"]).stdout,
        before
    );
    let refused = node.machine.toon(&["describe", "--amount", "1", "--json"]);
    assert_eq!(refused.exit_code, 2);
}
