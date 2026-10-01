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

/// The relay image `Cargo.toml` pins under `[package.metadata.toon]`.
fn pinned_relay_image() -> String {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("read Cargo.toml");
    let image = manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .skip_while(|line| *line != "[package.metadata.toon]")
        .take_while(|line| *line == "[package.metadata.toon]" || !line.starts_with('['))
        .find_map(|line| line.strip_prefix("relay_image = \""))
        .and_then(|rest| rest.split_once('"'))
        .map(|(image, _)| image)
        .expect("Cargo.toml pins a relay image");
    let (_, digest) = image
        .split_once("@sha256:")
        .expect("the relay image is pinned by digest");
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the relay image digest is a sha256: {image}"
    );
    image.to_string()
}

#[test]
fn the_version_with_json_names_the_connector_revision_and_relay_image() {
    let machine = Machine::new();

    let run = machine.toon(&["--version", "--json"]);

    assert_eq!(
        run.json(),
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "connector_revision": locked_connector_revision(),
            "relay_image": pinned_relay_image(),
        })
    );
    assert_eq!(run.exit_code, 0);
}

#[test]
fn the_version_as_text_names_the_connector_revision_and_relay_image() {
    let machine = Machine::new();

    let run = machine.toon(&["--version"]);

    assert_eq!(
        run.stdout,
        format!(
            "toon {} (connector {}, relay {})\n",
            env!("CARGO_PKG_VERSION"),
            locked_connector_revision(),
            pinned_relay_image()
        )
    );
    assert_eq!(run.exit_code, 0);
}
