//! What a command hands back: a report or an error, each with its exit code.

use serde_json::{json, Value};

/// The exit codes. They are part of the interface and never change meaning between
/// releases. `docs/exit-codes.md` documents them and `toon --help` lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    Success = 0,
    Failure = 1,
    Usage = 2,
    NoAgentNode = 3,
}

/// What a command found, in both renderings.
#[derive(Debug)]
pub struct Report {
    pub exit: Exit,
    pub json: Value,
    pub text: String,
}

/// Why a command did not do what was asked. `code` is stable; `message` is for reading.
#[derive(Debug)]
pub struct Error {
    pub exit: Exit,
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn json(&self) -> Value {
        json!({ "error": { "code": self.code, "message": self.message } })
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit as u8)
    }
}
