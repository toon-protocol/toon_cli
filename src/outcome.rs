//! What a command hands back: a report or an error, each with its exit code.

use std::process::ExitCode;

use serde_json::{json, Value};

/// The exit codes. They are part of the interface and never change meaning between
/// releases. `toon --help` lists them from here, and `docs/exit-codes.md` documents them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    Success = 0,
    Failure = 1,
    Usage = 2,
    NoAgentNode = 3,
}

impl Exit {
    pub const ALL: [Exit; 4] = [Exit::Success, Exit::Failure, Exit::Usage, Exit::NoAgentNode];

    pub fn meaning(self) -> &'static str {
        match self {
            Exit::Success => "The command did what was asked",
            Exit::Failure => "The command failed; the error's code says why",
            Exit::Usage => "The command line was not understood",
            Exit::NoAgentNode => "There is no agent node on this machine",
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit as u8)
    }
}

/// What a command found, in both renderings.
#[derive(Debug)]
pub struct Report {
    pub exit: Exit,
    pub json: Value,
    pub text: String,
}

/// The error codes. Like the exit codes they are stable, and `docs/exit-codes.md`
/// documents them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    Usage,
    HomeUnresolved,
    NoWallet,
    PassphraseMissing,
    PassphraseUnreadable,
    PassphraseWrong,
    KeystoreCorrupt,
    Io,
    NoAgentNode,
    ConnectorFailed,
    AlreadyRunning,
    NotRunning,
    SendFailed,
    ChainFailed,
    ChannelFailed,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "usage",
            ErrorCode::HomeUnresolved => "home_unresolved",
            ErrorCode::NoWallet => "no_wallet",
            ErrorCode::PassphraseMissing => "passphrase_missing",
            ErrorCode::PassphraseUnreadable => "passphrase_unreadable",
            ErrorCode::PassphraseWrong => "passphrase_wrong",
            ErrorCode::KeystoreCorrupt => "keystore_corrupt",
            ErrorCode::Io => "io",
            ErrorCode::NoAgentNode => "no_agent_node",
            ErrorCode::ConnectorFailed => "connector_failed",
            ErrorCode::AlreadyRunning => "already_running",
            ErrorCode::NotRunning => "not_running",
            ErrorCode::SendFailed => "send_failed",
            ErrorCode::ChainFailed => "chain_failed",
            ErrorCode::ChannelFailed => "channel_failed",
        }
    }

    pub fn exit(self) -> Exit {
        match self {
            ErrorCode::Usage => Exit::Usage,
            ErrorCode::NoWallet | ErrorCode::NoAgentNode => Exit::NoAgentNode,
            ErrorCode::HomeUnresolved
            | ErrorCode::PassphraseMissing
            | ErrorCode::PassphraseUnreadable
            | ErrorCode::PassphraseWrong
            | ErrorCode::KeystoreCorrupt
            | ErrorCode::Io
            | ErrorCode::ConnectorFailed
            | ErrorCode::AlreadyRunning
            | ErrorCode::NotRunning
            | ErrorCode::SendFailed
            | ErrorCode::ChainFailed
            | ErrorCode::ChannelFailed => Exit::Failure,
        }
    }
}

/// Why a command did not do what was asked. `message` is for reading and may be reworded.
#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

impl Error {
    pub fn json(&self) -> Value {
        json!({ "error": { "code": self.code.as_str(), "message": self.message } })
    }
}
