mod support;

use std::fs;
use std::path::Path;

use serde_json::Value;
use support::fake_chain::FakeChain;
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

    fn publish(&self, directory: &Path, draft: &str, extra: &[&str]) -> support::Run {
        let relay = self.relay_url();
        let mut args = vec!["nip", "publish", draft, "--relay", &relay, "--json"];
        args.extend_from_slice(extra);
        self.machine.toon_with(&args, |command| {
            command
                .current_dir(directory)
                .env("TOON_PASSPHRASE", support::PASSPHRASE);
        })
    }

    fn drafts(&self, identifier: &str) -> Vec<Value> {
        let filter = format!(r##"{{"kinds":[30817],"#d":["{identifier}"]}}"##);
        let query = self.machine.toon(&[
            "event",
            "query",
            &self.relay_url(),
            "--filter",
            &filter,
            "--json",
        ]);
        query.json()["events"].as_array().expect("events").clone()
    }
}

fn new_draft(machine: &Machine, directory: &Path, title: &str) -> support::Run {
    machine.toon_with(&["nip", "new", title, "--json"], |command| {
        command.current_dir(directory);
    })
}

fn tag<'a>(event: &'a Value, name: &str) -> Option<&'a Value> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == name)
        .map(|tag| &tag[1])
}

#[test]
fn new_writes_a_draft_from_the_template() {
    let machine = Machine::new();
    let directory = tempfile::tempdir().unwrap();

    let run = new_draft(&machine, directory.path(), "Ping Pong");

    assert_eq!(run.exit_code, 0, "{}", run.stdout);
    assert_eq!(run.json()["identifier"], "ping-pong");
    let draft = fs::read_to_string(directory.path().join("ping-pong.md")).unwrap();
    let template = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("nips")
            .join("TEMPLATE.md"),
    )
    .unwrap();
    assert_eq!(draft, template.replacen("# Title", "# Ping Pong", 1));
}

#[test]
fn new_does_not_overwrite_a_draft() {
    let machine = Machine::new();
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(new_draft(&machine, directory.path(), "Ping").exit_code, 0);
    fs::write(directory.path().join("ping.md"), "# Mine\n").unwrap();

    let run = new_draft(&machine, directory.path(), "Ping");

    assert_eq!(run.json()["error"]["code"], "draft_refused");
    assert_eq!(run.exit_code, 1);
    assert_eq!(
        fs::read_to_string(directory.path().join("ping.md")).unwrap(),
        "# Mine\n"
    );
}

#[test]
fn a_draft_is_published_as_the_event_the_proposals_draft_specifies() {
    let node = running();
    let _ = &node.up;
    let directory = tempfile::tempdir().unwrap();
    let document = "# Ping Pong\n\n`draft` `optional`\n\nTwo agents\ncan ping.\n\n## Kinds\n\n\
        | Kind | Event | Class | Signed with | Defined by |\n| --- | --- | --- | --- | --- |\n\
        | `9000` | Ping | regular | Anyone | This draft |\n\
        | `1` | Note | regular | Anyone | NIP-01 |\n";
    fs::write(directory.path().join("ping-pong.md"), document).unwrap();

    let published = node.publish(directory.path(), "ping-pong.md", &["--topic", "agents"]);

    assert_eq!(published.exit_code, 0, "{}", published.stdout);
    assert_eq!(published.json()["outcome"], "published");
    let stored = node.drafts("ping-pong");
    assert_eq!(stored.len(), 1, "{stored:?}");
    let event = &stored[0];
    assert_eq!(event, &published.json()["event"]);
    assert_eq!(event["kind"], 30817);
    assert_eq!(event["pubkey"], node.agent_identity());
    assert_eq!(event["content"], document);
    assert_eq!(
        event["tags"],
        serde_json::json!([
            ["d", "ping-pong"],
            ["title", "Ping Pong"],
            ["k", "9000", "Ping"],
            ["summary", "Two agents can ping."],
            ["t", "agents"]
        ])
    );
}

#[test]
fn publishing_a_changed_draft_replaces_the_earlier_one() {
    let node = running();
    let _ = &node.up;
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        new_draft(&node.machine, directory.path(), "Ping").exit_code,
        0
    );
    let first = node.publish(directory.path(), "ping.md", &[]);
    assert_eq!(first.exit_code, 0, "{}", first.stdout);

    let path = directory.path().join("ping.md");
    let changed = format!("{}\nAnd more.\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, &changed).unwrap();
    let second = node.publish(directory.path(), "ping.md", &[]);

    assert_eq!(second.exit_code, 0, "{}", second.stdout);
    let stored = node.drafts("ping");
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0]["content"], changed);
    assert!(
        stored[0]["created_at"].as_u64() > first.json()["event"]["created_at"].as_u64(),
        "the revision is made later than the one it replaces"
    );
}

#[test]
fn a_draft_whose_title_changed_is_refused_unless_told() {
    let node = running();
    let _ = &node.up;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ping.md");
    fs::write(&path, "# Ping\n").unwrap();
    assert_eq!(node.publish(directory.path(), "ping.md", &[]).exit_code, 0);
    fs::write(&path, "# Something else\n").unwrap();

    let refused = node.publish(directory.path(), "ping.md", &[]);

    assert_eq!(refused.json()["error"]["code"], "draft_refused");
    assert_eq!(refused.exit_code, 1);
    assert_eq!(tag(&node.drafts("ping")[0], "title").unwrap(), "Ping");

    let told = node.publish(directory.path(), "ping.md", &["--title-changed"]);
    assert_eq!(told.exit_code, 0, "{}", told.stdout);
    assert_eq!(
        tag(&node.drafts("ping")[0], "title").unwrap(),
        "Something else"
    );
}

#[test]
fn a_file_that_does_not_name_a_draft_is_refused() {
    let machine = Machine::new();
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("Bad_Name.md"), "# T\n").unwrap();
    fs::write(directory.path().join("untitled.md"), "no title\n").unwrap();

    for file in ["Bad_Name.md", "untitled.md"] {
        let run = machine.toon_with(
            &[
                "nip",
                "publish",
                file,
                "--relay",
                "ws://127.0.0.1:1",
                "--json",
            ],
            |command| {
                command.current_dir(directory.path());
            },
        );
        assert_eq!(run.json()["error"]["code"], "draft_refused", "{file}");
        assert_eq!(run.exit_code, 1);
    }
}
