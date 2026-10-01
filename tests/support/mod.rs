//! The command-line test harness: the one seam the spec names.
//!
//! A test runs the built `toon` binary the way an operator does, against a home
//! directory of its own, and asserts on what the operator could observe: the output,
//! the exit code, and the files left under that home.

// Each test file compiles this module separately and uses a different part of it.
#![allow(dead_code)]

pub mod anvil_chain;
pub mod fake_chain;
pub mod stub_app;
pub mod unpeerable;

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

/// How long a foreground `toon` gets to print a line or to exit. A connector binds to
/// its chain before it listens, so this is generous.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The wallet passphrase the tests use.
pub const PASSPHRASE: &str = "correct horse battery staple";

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
        let mut command = self.command(args);
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

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toon"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.home())
            .current_dir(self.home())
            .stdin(Stdio::null());
        command
    }

    /// Run `toon init` for an agent node that settles on `chain`, with the passphrase
    /// every test uses.
    pub fn init_on(&self, chain: &fake_chain::FakeChain) -> Run {
        self.init_on_with(chain, &[])
    }

    /// Like `init_on`, for an agent node whose relay is served at `relay_url`.
    pub fn init_on_serving(&self, chain: &fake_chain::FakeChain, relay_url: &str) -> Run {
        self.init_on_with(chain, &["--relay-url", relay_url])
    }

    /// Like `init_on_serving`, on the local chain `anvil`: its token moves by ERC-3009.
    pub fn init_on_anvil(&self, chain: &anvil_chain::AnvilChain, relay_url: &str) -> Run {
        let decimals = anvil_chain::TOKEN_DECIMALS.to_string();
        let rpc_url = chain.rpc_url();
        let token = chain.token();
        self.toon_with(
            &[
                "init",
                "--json",
                "--evm-rpc-url",
                &rpc_url,
                "--evm-token",
                &token,
                "--evm-decimals",
                &decimals,
                "--relay-url",
                relay_url,
                // The chain and the connectors share one machine.
                "--allow-plaintext-peers",
            ],
            |command| {
                command.env("TOON_PASSPHRASE", PASSPHRASE);
            },
        )
    }

    fn init_on_with(&self, chain: &fake_chain::FakeChain, more: &[&str]) -> Run {
        let decimals = fake_chain::TOKEN_DECIMALS.to_string();
        let rpc_url = chain.rpc_url();
        let mut args = vec![
            "init",
            "--json",
            "--evm-rpc-url",
            &rpc_url,
            "--evm-token",
            fake_chain::TOKEN,
            "--evm-decimals",
            &decimals,
            "--evm-asset-name",
            "USDC",
            "--evm-asset-version",
            "2",
            "--evm-transfer-method",
            "permit2",
        ];
        args.extend_from_slice(more);
        self.toon_with(&args, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    }

    /// Write `contents` to `name` in the agent node's home, and return its path.
    pub fn write_agent_node_file(&self, name: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.agent_node_home().join(name);
        fs::create_dir_all(self.agent_node_home()).expect("create the agent node's home");
        fs::write(&path, contents).expect("write a file in the agent node's home");
        path
    }

    /// Start `toon` with `args` and leave it running, for a command that stays in the
    /// foreground. It is killed when the returned value is dropped.
    pub fn start(&self, args: &[&str]) -> Foreground {
        let stderr = self.home().join("toon.stderr");
        let mut child = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(File::create(&stderr).expect("create a file for stderr"))
            .spawn()
            .expect("start the toon binary");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let (sender, lines) = mpsc::channel();
        // Read on a thread of its own, so a `toon` that prints nothing fails the test
        // instead of hanging it.
        thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Foreground {
            child,
            lines,
            stderr,
        }
    }
}

/// A `toon` that was started and left running.
pub struct Foreground {
    child: Child,
    lines: Receiver<String>,
    stderr: PathBuf,
}

impl Foreground {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The next line of standard output as the one JSON document `--json` promises,
    /// which a foreground command prints once what it runs is up.
    pub fn report(&self) -> Value {
        let line = self.line();
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("the report is not a JSON document ({error}):\n{line}"))
    }

    /// The next line of standard output, for a command that prints text.
    pub fn line(&self) -> String {
        self.lines
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("toon printed nothing; stderr:\n{}", self.stderr()))
    }

    /// Wait for it to exit by itself, and return its exit code.
    pub fn exit_code(&mut self) -> i32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for toon") {
                return status
                    .code()
                    .expect("toon exited without being killed by a signal");
            }
            assert!(Instant::now() < deadline, "toon is still running");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// What it has written to standard error so far.
    pub fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).expect("read stderr")
    }

    /// Kill it the way a crash or `kill -9` would: with no chance to clean up.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        self.kill();
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
