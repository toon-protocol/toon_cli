mod support;

use serde_json::Value;
use std::time::Duration;
use support::fake_chain::FakeChain;
use support::fake_remote_relay::FakeRemoteRelay;
use support::{Foreground, Machine};

/// An agent node, running, with the fake relay behind its connector.
struct Running {
    machine: Machine,
    up: Foreground,
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
        up,
        _chain: chain,
    }
}

impl Running {
    fn publish(&self, args: &[&str]) -> support::Run {
        let mut all = vec!["event", "publish", "--json"];
        all.extend_from_slice(args);
        self.machine.toon_with(&all, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    /// The websocket URL of the relay: the fake serves it on its read port.
    fn relay_url(&self) -> String {
        let status = self.machine.toon(&["status", "--json"]).json();
        let address = status["agent_node"]["toon_apps"][0]["apps"][0]["read_address"]
            .as_str()
            .unwrap_or_else(|| panic!("the relay has no read address: {status}"));
        format!("ws://{address}")
    }

    fn agent_identity(&self) -> String {
        let show = self
            .machine
            .toon_with(&["wallet", "show", "--json"], |command| {
                command.env("TOON_PASSPHRASE", support::PASSPHRASE);
            });
        show.json()["wallet"]["agent_identity"]
            .as_str()
            .expect("the agent identity")
            .to_owned()
    }
}

#[test]
fn the_relay_is_read_on_its_read_port_and_status_says_where() {
    let node = running();
    let status = node.machine.toon(&["status", "--json"]).json();
    let relay = &status["agent_node"]["toon_apps"][0]["apps"][0];
    let write = relay["address"].as_str().unwrap();
    let read = relay["read_address"].as_str().unwrap();
    assert_ne!(write, read);
    let text = node.machine.toon(&["status"]).stdout;
    assert!(text.contains(&format!("ws://{read}")), "{text}");

    // The write port refuses a websocket upgrade, as the relay's image does.
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(write).unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {write}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
}

#[test]
fn a_published_event_is_read_back_from_the_relay() {
    let node = running();
    let _ = &node.up;

    let published = node.publish(&[
        "--kind",
        "1",
        "--content",
        "hello toon",
        "--tags",
        r#"[["t","toon"]]"#,
    ]);

    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    let report = published.json();
    assert_eq!(report["outcome"], "published", "{report}");
    let event = report["event"].clone();
    assert_eq!(event["pubkey"], node.agent_identity());
    assert_eq!(event["kind"], 1);
    assert_eq!(event["content"], "hello toon");
    assert_eq!(event["tags"][0][1], "toon");

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
        "--json",
    ]);

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    assert_eq!(query.stderr, "");
    let events = query.json()["events"].clone();
    assert_eq!(events, Value::Array(vec![event]));
}

#[test]
fn a_query_returns_only_the_events_the_filter_matches() {
    let node = running();
    assert_eq!(
        node.publish(&["--kind", "1", "--content", "a"]).exit_code,
        0
    );
    assert_eq!(
        node.publish(&["--kind", "7", "--content", "b"]).exit_code,
        0
    );

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[7]}"#,
        "--json",
    ]);

    let events = query.json()["events"].clone();
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    assert_eq!(events[0]["content"], "b");
}

#[test]
fn a_query_that_matches_nothing_says_so() {
    let node = running();

    let query = node.machine.toon(&[
        "event",
        "query",
        &node.relay_url(),
        "--filter",
        r#"{"kinds":[1]}"#,
    ]);

    assert_eq!(query.stdout, "No stored event matches.\n");
    assert_eq!(query.exit_code, 0);
}

#[test]
fn publish_needs_the_agent_node_to_be_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon_with(&["event", "publish", "--kind", "1", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });

    assert_eq!(run.json()["error"]["code"], "not_running");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn publish_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "publish", "--kind", "1", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}

#[test]
fn publish_refuses_tags_that_are_not_arrays_of_strings() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "publish", "--kind", "1", "--tags", "[1]", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn query_refuses_a_filter_that_is_not_an_object() {
    let machine = Machine::new();

    let run = machine.toon(&[
        "event",
        "query",
        "ws://127.0.0.1:1",
        "--filter",
        "[]",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "usage");
    assert_eq!(run.exit_code, 2);
}

#[test]
fn query_of_a_relay_that_is_not_there_fails() {
    let machine = Machine::new();

    let run = machine.toon(&[
        "event",
        "query",
        "ws://127.0.0.1:1",
        "--filter",
        "{}",
        "--json",
    ]);

    assert_eq!(run.json()["error"]["code"], "query_failed");
    assert_eq!(run.exit_code, 1);
}

const OTHER: &str = "cd0000000000000000000000000000000000000000000000000000000000cd00";

impl Running {
    fn follow(&self, keys: &[&str]) {
        let tags: Vec<Value> = keys
            .iter()
            .map(|key| serde_json::json!(["p", key]))
            .collect();
        let published = self.publish(&["--kind", "3", "--tags", &Value::Array(tags).to_string()]);
        assert_eq!(published.exit_code, 0, "{}", published.stdout);
    }

    fn query(&self, args: &[&str]) -> support::Run {
        let url = self.relay_url();
        let mut all = vec!["event", "query", &url[..], "--json"];
        all.extend_from_slice(args);
        self.machine.toon_with(&all, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }
}

#[test]
fn following_queries_only_the_events_of_the_keys_in_the_follow_list() {
    let node = running();
    let me = node.agent_identity();
    assert_eq!(
        node.publish(&["--kind", "1", "--content", "mine"])
            .exit_code,
        0
    );
    node.follow(&[&me]);

    let query = node.query(&["--following", "--filter", r#"{"kinds":[1]}"#]);

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    let report = query.json();
    assert_eq!(report["filter"]["kinds"], serde_json::json!([1]));
    assert_eq!(report["filter"]["authors"], serde_json::json!([me]));
    let events = report["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{report}");
    assert_eq!(events[0]["content"], "mine");
}

#[test]
fn following_without_a_filter_leaves_out_the_events_of_other_keys() {
    let node = running();
    assert_eq!(
        node.publish(&["--kind", "1", "--content", "mine"])
            .exit_code,
        0
    );
    node.follow(&[OTHER]);

    let query = node.query(&["--following"]);

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    assert_eq!(query.json()["events"], serde_json::json!([]));
    assert_eq!(
        query.json()["filter"]["authors"],
        serde_json::json!([OTHER])
    );
}

#[test]
fn following_with_a_filter_that_has_authors_is_usage() {
    let node = running();
    node.follow(&[OTHER]);

    let query = node.query(&["--following", "--filter", r#"{"authors":["ab"]}"#]);

    assert_eq!(query.json()["error"]["code"], "usage");
    assert_eq!(query.exit_code, 2, "{}", query.stdout);
}

#[test]
fn following_with_no_follow_list_or_an_empty_one_is_no_follow_list() {
    let node = running();

    let none = node.query(&["--following"]);
    assert_eq!(none.json()["error"]["code"], "no_follow_list");
    assert_eq!(none.exit_code, 1);

    node.follow(&[]);
    let empty = node.query(&["--following"]);
    assert_eq!(empty.json()["error"]["code"], "no_follow_list");
}

#[test]
fn subscribing_with_no_follow_list_pays_nothing() {
    let node = running();

    let run = node.machine.toon_with(
        &[
            "relay",
            "subscribe",
            "ws://127.0.0.1:9",
            "--following",
            "--amount",
            "1000",
            "--yes",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(run.json()["error"]["code"], "no_follow_list");
    assert_eq!(run.exit_code, 1);
}

#[test]
fn following_reads_the_newest_follow_list() {
    let node = running();
    let me = node.agent_identity();
    node.follow(&[OTHER]);
    // A kind 3 a second later, so it is the newer.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    node.follow(&[&me]);

    let query = node.query(&["--following"]);

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    assert_eq!(query.json()["filter"]["authors"], serde_json::json!([me]));
}

#[test]
fn following_queries_a_remote_relay_for_the_events_of_the_followed_keys() {
    let node = running();
    let me = node.agent_identity();
    node.follow(&[OTHER]);
    let remote = FakeRemoteRelay::start("g.toon.subscribe", 1000, 10);
    for (number, author) in [(1u64, OTHER), (2, &me[..])] {
        remote.broadcast(serde_json::json!({
            "id": format!("{number:064x}"),
            "pubkey": author,
            "created_at": 1_790_000_000 + number,
            "kind": 1,
            "tags": [],
            "content": format!("event {number}"),
            "sig": "00".repeat(64),
        }));
    }

    let query = node.machine.toon_with(
        &["event", "query", &remote.url(), "--following", "--json"],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(query.exit_code, 0, "{}", query.stdout);
    let report = query.json();
    let events = report["events"].as_array().expect("events");
    assert_eq!(events.len(), 1, "{report}");
    assert_eq!(events[0]["pubkey"], OTHER);
}

#[test]
fn following_with_the_own_relay_not_running_is_not_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let query = machine.toon_with(
        &[
            "event",
            "query",
            "ws://127.0.0.1:9",
            "--following",
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(
        query.json()["error"]["code"],
        "not_running",
        "{}",
        query.stdout
    );
    assert_eq!(query.exit_code, 1);
}

impl Running {
    /// `toon event watch` with `args`, left running.
    fn watch(&self, args: &[&str]) -> Foreground {
        let mut all = vec!["event", "watch"];
        all.extend_from_slice(args);
        self.machine.start_with(&all, |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    /// Publish kind 1 events, `<prefix>0`, `<prefix>1` and so on, until `watch` prints one:
    /// it says nothing of being connected, and prints only what arrives after. The events
    /// it printed, as documents.
    fn publish_until_printed(&self, watch: &Foreground, prefix: &str) -> Vec<Value> {
        for number in 0..100 {
            let content = format!("{prefix}{number}");
            let published = self.publish(&["--kind", "1", "--content", &content]);
            assert_eq!(published.exit_code, 0, "{}", published.stdout);
            if let Some(line) = watch.try_line(Duration::from_millis(300)) {
                let mut printed = vec![serde_json::from_str(&line).expect("one document")];
                while let Some(line) = watch.try_line(Duration::from_millis(300)) {
                    printed.push(serde_json::from_str(&line).expect("one document"));
                }
                return printed;
            }
        }
        panic!("the watch printed nothing; stderr:\n{}", watch.stderr());
    }

    /// Start selling the relay's live feed, which restarts the relay.
    fn sell_the_feed(&self) {
        let run = self.machine.toon(&[
            "relay",
            "price",
            "--subscribe",
            "1000",
            "--broadcast",
            "10",
            "--yes",
            "--json",
        ]);
        assert_eq!(run.exit_code, 0, "{}", run.stdout);
    }

    fn limits(&self) -> Value {
        self.machine.toon(&["limit", "show", "--json"]).json()["limits"].clone()
    }
}

#[test]
fn watch_prints_an_event_written_after_it_started_and_not_the_stored_ones() {
    let node = running();
    assert_eq!(
        node.publish(&["--kind", "1", "--content", "before"])
            .exit_code,
        0
    );
    let limits = node.limits();
    let watch = node.watch(&[]);

    let printed = node.publish_until_printed(&watch, "after");

    assert!(
        printed
            .iter()
            .all(|event| event["content"].as_str().unwrap().starts_with("after")),
        "{printed:?}"
    );
    assert_eq!(printed[0]["kind"], 1);
    // It pays nothing and counts nothing against the limit but what was published: 0.
    assert_eq!(node.limits(), limits);
}

#[test]
fn watch_with_a_filter_prints_only_the_live_events_that_match() {
    let node = running();
    let watch = node.watch(&["--filter", r#"{"kinds":[7],"limit":5}"#]);

    for number in 0..100 {
        assert_eq!(
            node.publish(&["--kind", "1", "--content", "note"])
                .exit_code,
            0
        );
        let reaction = format!("+{number}");
        assert_eq!(
            node.publish(&["--kind", "7", "--content", &reaction])
                .exit_code,
            0
        );
        if let Some(line) = watch.try_line(Duration::from_millis(300)) {
            let printed: Value = serde_json::from_str(&line).expect("one document");
            assert_eq!(printed["kind"], 7, "{printed}");
            while let Some(line) = watch.try_line(Duration::from_millis(300)) {
                let printed: Value = serde_json::from_str(&line).expect("one document");
                assert_eq!(printed["kind"], 7, "{printed}");
            }
            return;
        }
    }
    panic!("the watch printed nothing; stderr:\n{}", watch.stderr());
}

#[test]
fn watch_following_prints_only_the_live_events_of_followed_keys() {
    let node = running();
    let me = node.agent_identity();
    node.follow(&[&me]);
    let watch = node.watch(&["--following"]);

    let printed = node.publish_until_printed(&watch, "mine");

    assert!(printed.iter().all(|event| event["pubkey"] == me.as_str()));
    // The filter is the follow list: with another key followed, nothing of mine is printed.
    // A follow list replaces the one before it only when it is newer: of two written in the
    // same second the relay keeps the lower id, so this one waits for the next second.
    std::thread::sleep(Duration::from_millis(1100));
    node.follow(&[OTHER]);
    let other = node.watch(&["--following"]);
    for number in 0..5 {
        let content = format!("not-followed{number}");
        node.publish(&["--kind", "1", "--content", &content]);
    }
    assert_eq!(other.try_line(Duration::from_millis(500)), None);
}

#[test]
fn watch_following_with_a_filter_that_has_authors_is_usage() {
    let node = running();
    node.follow(&[OTHER]);

    let run = node.machine.toon_with(
        &[
            "event",
            "watch",
            "--following",
            "--filter",
            r#"{"authors":["ab"]}"#,
            "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );

    assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
    assert_eq!(run.exit_code, 2);
}

#[test]
fn watch_following_with_no_follow_list_or_an_empty_one_is_no_follow_list() {
    let node = running();
    let watch = |node: &Running| {
        node.machine
            .toon_with(&["event", "watch", "--following", "--json"], |command| {
                command.env("TOON_PASSPHRASE", support::PASSPHRASE);
            })
    };

    let none = watch(&node);
    assert_eq!(none.json()["error"]["code"], "no_follow_list");
    assert_eq!(none.exit_code, 1);

    node.follow(&[]);
    assert_eq!(watch(&node).json()["error"]["code"], "no_follow_list");
}

#[test]
fn watch_refuses_a_filter_that_is_not_an_object() {
    let node = running();

    let run = node
        .machine
        .toon(&["event", "watch", "--filter", "[]", "--json"]);

    assert_eq!(run.json()["error"]["code"], "usage", "{}", run.stdout);
    assert_eq!(run.exit_code, 2);
}

#[test]
fn watch_reads_a_relay_that_sells_its_feed_as_its_operator_with_no_subscription() {
    let node = running();
    node.sell_the_feed();
    let watch = node.watch(&[]);

    let printed = node.publish_until_printed(&watch, "operator");

    assert!(printed[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("operator"));
}

#[test]
fn watch_names_the_public_host_of_a_relay_that_sells_its_feed_on_clearnet() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on_clearnet(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let _ = up.report();
    let node = Running {
        machine,
        up,
        _chain: chain,
    };
    node.sell_the_feed();
    let watch = node.watch(&[]);

    // The relay checks the `relay` tag against toon.example.com, and dialled loopback.
    let printed = node.publish_until_printed(&watch, "clearnet");

    assert!(printed[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("clearnet"));
}

#[test]
fn watch_with_the_own_relay_not_running_is_not_running() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);

    let run = machine.toon(&["event", "watch", "--json"]);

    assert_eq!(run.json()["error"]["code"], "not_running", "{}", run.stdout);
    assert_eq!(run.exit_code, 1);
}

#[test]
fn watch_on_a_machine_with_no_agent_node_says_so() {
    let machine = Machine::new();

    let run = machine.toon(&["event", "watch", "--json"]);

    assert_eq!(run.json()["error"]["code"], "no_agent_node");
    assert_eq!(run.exit_code, 3);
}
