//! `toon status`.

use std::path::Path;

use serde_json::json;

use crate::outcome::{Exit, Report};

/// The agent node at `home`.
///
/// No command creates an agent node yet, so there is never one to find. `init` brings
/// the state this will read.
pub fn status(home: &Path) -> Report {
    let home = home.to_string_lossy();
    Report {
        exit: Exit::NoAgentNode,
        json: json!({ "home": home, "agent_node": null }),
        text: format!("No agent node at {home}."),
    }
}
