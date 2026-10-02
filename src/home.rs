//! Where the agent node on this machine lives.

use std::env;
use std::path::PathBuf;

use crate::outcome::{Error, ErrorCode};

/// The agent node's home directory: `~/.toon/agent-node`.
///
/// `toon-client` keeps its own files directly under `~/.toon`, so the agent node gets
/// a directory of its own beside them.
pub fn resolve() -> Result<PathBuf, Error> {
    user().map(|home| home.join(".toon").join("agent-node"))
}

/// The user's home directory: `$HOME`.
pub fn user() -> Result<PathBuf, Error> {
    match env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => Ok(PathBuf::from(home)),
        None => Err(Error {
            nothing_sent: false,
            code: ErrorCode::HomeUnresolved,
            message: "HOME is not set or is empty, so there is nowhere to look for an agent node."
                .into(),
        }),
    }
}
