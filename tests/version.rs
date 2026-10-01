mod support;

use std::fs;
use std::path::Path;

use serde_json::json;
use support::Machine;

/// The connector commit Cargo resolved the pinned dependency to, read from `Cargo.lock`:
/// the commit the binary under test was built from.
fn locked_connector_revision() -> String {
    let lock = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))
        .expect("read Cargo.lock");
    let mut revisions: Vec<&str> = lock
        .lines()
        .filter(|line| line.contains("git+https://github.com/toon-protocol/connector?"))
        .filter_map(|line| line.rsplit_once('#'))
        .map(|(_, revision)| revision.trim_end_matches('"'))
        .collect();
    revisions.dedup();
    assert_eq!(
        revisions.len(),
        1,
        "every connector crate is locked to one commit: {revisions:?}"
    );
    revisions[0].to_string()
}

#[test]
fn the_version_with_json_names_the_connector_revision_it_embeds() {
    let machine = Machine::new();

    let run = machine.toon(&["--version", "--json"]);

    assert_eq!(
        run.json(),
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "connector_revision": locked_connector_revision(),
        })
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn the_version_as_text_names_the_connector_revision_it_embeds() {
    let machine = Machine::new();

    let run = machine.toon(&["--version"]);

    assert_eq!(
        run.stdout,
        format!(
            "toon {} (connector {})\n",
            env!("CARGO_PKG_VERSION"),
            locked_connector_revision()
        )
    );
    assert_eq!(run.exit_code, 0);
}
