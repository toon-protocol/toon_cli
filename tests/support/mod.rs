//! The command-line test harness: the one seam the spec names.
//!
//! A test runs the built `toon` binary the way an operator does, against a home
//! directory of its own, and asserts on what the operator could observe: the output,
//! the exit code, and the files left under that home.

// Each test file compiles this module separately and uses a different part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

/// One operator's machine: an empty home directory that is deleted on drop.
pub struct Machine {
    home: TempDir,
}

/// What one run of `toon` printed and how it exited.
#[derive(Debug)]
pub struct Run {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Machine {
    pub fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("create a temporary home directory"),
        }
    }

    /// The operator's home directory (`$HOME`).
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Where `toon` keeps this machine's agent node.
    pub fn agent_node_home(&self) -> PathBuf {
        self.home().join(".toon").join("agent-node")
    }

    /// Run `toon` with `args`.
    pub fn toon(&self, args: &[&str]) -> Run {
        self.toon_with(args, |_| {})
    }

    /// Run `toon` with `args`, after `configure` has adjusted the command.
    ///
    /// The environment is empty apart from `HOME`, so nothing leaks in from the
    /// machine the tests run on. Standard input is closed: a command that prompts
    /// fails here instead of hanging.
    pub fn toon_with(&self, args: &[&str], configure: impl FnOnce(&mut Command)) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toon"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.home())
            .current_dir(self.home())
            .stdin(Stdio::null());
        configure(&mut command);
        let output = command.output().expect("run the toon binary");
        Run {
            exit_code: output
                .status
                .code()
                .expect("toon exited without being killed by a signal"),
            stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
            stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        }
    }
}

impl Run {
    /// Standard output as the one JSON document `--json` promises.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not one JSON document ({error}):\n{}",
                self.stdout
            )
        })
    }
}
