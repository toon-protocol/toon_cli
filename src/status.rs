//! `toon status`.

use std::path::Path;

use serde_json::json;

use crate::outcome::{Exit, Report};

/// The agent node at `home`.
///
/// `toon init` creates the wallet, but status does not read it yet: an agent node is a
/// wallet and its TOON apps, and no command creates a TOON app yet.
pub fn status(home: &Path) -> Report {
    let home = home.to_string_lossy();
    Report {
        exit: Exit::NoAgentNode,
        json: json!({ "home": home, "agent_node": null }),
        text: format!("No agent node at {home}."),
    }
}
