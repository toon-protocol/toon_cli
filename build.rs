//! Reads the connector revision this binary embeds out of `Cargo.toml`, so the pin is
//! stated there and nowhere else: `toon --version` reports what this finds.
//!
//! Cargo has no way to share one `rev` between dependency lines, so every line that
//! names the connector's repository must carry the same one. The build fails otherwise.

use std::env;
use std::fs;
use std::path::Path;

const CONNECTOR_REPOSITORY: &str = "github.com/toon-protocol/connector\"";

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let manifest =
        fs::read_to_string(Path::new(&manifest_dir).join("Cargo.toml")).expect("read Cargo.toml");

    let mut revisions: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && line.contains(CONNECTOR_REPOSITORY))
        .map(|line| {
            line.split_once("rev = \"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(revision, _)| revision)
                .unwrap_or_else(|| panic!("a connector dependency has no `rev`: {line}"))
        })
        .collect();
    revisions.dedup();

    match revisions.as_slice() {
        [revision] if is_full_revision(revision) => {
            println!("cargo:rustc-env=TOON_CONNECTOR_REVISION={revision}");
        }
        [revision] => panic!("the connector `rev` is not a full 40-character commit: {revision}"),
        [] => panic!("Cargo.toml has no dependency on the connector's repository"),
        several => panic!("the connector dependencies name different revisions: {several:?}"),
    }
}

fn is_full_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}
