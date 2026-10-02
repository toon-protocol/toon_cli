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
/// documents them. The list is `error_codes.table`, a line to a code, so that codes added
/// side by side merge.
macro_rules! error_codes {
    ($($variant:ident => $name:literal => $exit:ident,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ErrorCode {
            $($variant,)*
        }

        impl ErrorCode {
            #[cfg(test)]
            pub const ALL: &[ErrorCode] = &[$(ErrorCode::$variant,)*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $name,)*
                }
            }

            pub fn exit(self) -> Exit {
                match self {
                    $(ErrorCode::$variant => Exit::$exit,)*
                }
            }
        }
    };
}

include!("error_codes.table");

/// Why a command did not do what was asked. `message` is for reading and may be reworded.
#[derive(Debug)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// Set when the command certainly failed before it sent a packet, so that what it was
    /// to carry was never at risk. It is not part of the output.
    pub nothing_sent: bool,
    /// Set when a packet went unanswered: what it cost, and the event it carried.
    pub unanswered: Option<Unanswered>,
}

/// What a packet that the connector did not answer within the wait cost, and carried.
#[derive(Debug)]
pub struct Unanswered {
    /// What the outbound channels' watermarks moved by, at most what was sent.
    pub paid: u128,
    pub event: Option<Value>,
}

impl Error {
    pub fn json(&self) -> Value {
        let mut json = json!({ "error": { "code": self.code.as_str(), "message": self.message } });
        if let Some(unanswered) = &self.unanswered {
            json["paid"] = json!(unanswered.paid);
            if let Some(event) = &unanswered.event {
                json["event"] = event.clone();
            }
        }
        json
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    /// The binary's error codes are the ones `tests/error_codes.txt` fixes, which
    /// `tests/exit_codes.rs` checks against `docs/exit-codes.md`.
    #[test]
    fn the_error_codes_are_the_ones_the_tests_fix() {
        let fixed = include_str!("../tests/error_codes.txt");
        let mut fixed: Vec<&str> = fixed.lines().filter(|line| !line.is_empty()).collect();
        let mut codes: Vec<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
        fixed.sort_unstable();
        codes.sort_unstable();
        assert_eq!(codes, fixed);
    }
}
