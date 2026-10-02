//! The command-line test harness: the one seam the spec names.
//!
//! A test runs the built `toon` binary the way an operator does, against a home
//! directory of its own, and asserts on what the operator could observe: the output,
//! the exit code, and the files left under that home.

// Each test file compiles this module separately and uses a different part of it.
#![allow(dead_code)]

pub mod anvil_chain;
pub mod fake_chain;
pub mod fake_faucet;
pub mod fake_remote_relay;
pub mod local_chain;
pub mod spy;
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

/// The sealing key the connector at `url` answers with for its identity, as it gives it.
pub fn seal_key(url: &str) -> String {
    let identity: Value = reqwest::blocking::get(format!("{url}/identity"))
        .and_then(|response| response.json())
        .expect("the connector's identity");
    identity["publicKey"]
        .as_str()
        .expect("a publicKey")
        .to_owned()
}

/// A `connector_seal_key` that is 65 bytes beginning `04`, so it is read as a key, and is
/// no point on the curve, so nothing can be sealed to it.
pub const UNSEALABLE_KEY: &str =
    "04abababababababababababababababababababababababababababababababab\
                                  abababababababababababababababababababababababababababababababab";

/// The wallet passphrase the tests use.
pub const PASSPHRASE: &str = "correct horse battery staple";

/// The fake relay of `examples/fake_relay.rs`, which cargo builds for the tests.
pub fn fake_relay() -> PathBuf {
    let tests = std::env::current_exe().expect("the test binary's path");
    let program = tests
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps/<test>")
        .join("examples")
        .join("fake_relay");
    assert!(program.exists(), "{} is not built", program.display());
    program
}

/// One operator's machine: an empty home directory that is deleted on drop.
pub struct Machine {
    root: TempDir,
    home: PathBuf,
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
        let root = tempfile::tempdir().expect("create a temporary home directory");
        let home = root.path().to_path_buf();
        Self { root, home }
    }

    /// A machine whose home path is long enough that the agent node's control socket,
    /// `<home>/.toon/agent-node/supervisor.sock`, is well over the 107 bytes a Unix
    /// socket address holds.
    pub fn with_long_home() -> Self {
        let root = tempfile::tempdir().expect("create a temporary home directory");
        let mut home = root.path().to_path_buf();
        while home
            .join(".toon/agent-node/supervisor.sock")
            .as_os_str()
            .len()
            < 200
        {
            home.push("a-directory-with-a-rather-long-name");
        }
        std::fs::create_dir_all(&home).expect("create a home with a long path");
        Self { root, home }
    }

    /// The operator's home directory (`$HOME`).
    pub fn home(&self) -> &Path {
        &self.home
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
            // Apps run as local processes of the fake relay, never as containers.
            .env("TOON_APP_COMMAND", fake_relay())
            // The overlay is the loopback stand-in, never the Anyone daemon.
            .env("TOON_OVERLAY", "loopback")
            .current_dir(self.home())
            .stdin(Stdio::null());
        command
    }

    /// Run `toon init --json` with `args` after it, and the passphrase every test uses.
    pub fn init_with(&self, args: &[&str]) -> Run {
        let mut all = vec!["init", "--json", "--accept-anyone-terms"];
        all.extend_from_slice(args);
        self.toon_with(&all, |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        })
    }

    /// Run `toon init` for an agent node that settles on `chain`, with the passphrase
    /// every test uses.
    pub fn init_on(&self, chain: &fake_chain::FakeChain) -> Run {
        self.toon_with(
            &[
                "init",
                "--json",
                "--accept-anyone-terms",
                "--evm-rpc-url",
                &chain.rpc_url(),
                "--evm-token",
                fake_chain::TOKEN,
                "--evm-decimals",
                &fake_chain::TOKEN_DECIMALS.to_string(),
                "--evm-asset-name",
                "USDC",
                "--evm-asset-version",
                "2",
                "--evm-transfer-method",
                "permit2",
            ],
            |command| {
                command.env("TOON_PASSPHRASE", PASSPHRASE);
            },
        )
    }

    /// Like `init_on`, for a connector that is reached on clearnet: a connector peers over
    /// plain HTTP at the address it listens on, which a hidden service does not.
    pub fn init_on_clearnet(&self, chain: &fake_chain::FakeChain) -> Run {
        self.init_with(&[
            "--clearnet",
            "toon.example.com",
            "--evm-rpc-url",
            &chain.rpc_url(),
            "--evm-token",
            fake_chain::TOKEN,
            "--evm-decimals",
            &fake_chain::TOKEN_DECIMALS.to_string(),
            "--evm-asset-name",
            "USDC",
            "--evm-asset-version",
            "2",
            "--evm-transfer-method",
            "permit2",
        ])
    }

    /// Like `init_on`, on the local chain `anvil`: its token moves by ERC-3009, as the
    /// profile's does. The chain and the connectors share one machine, so a connector
    /// that peers needs `plaintext_peers`.
    pub fn init_on_anvil(&self, chain: &anvil_chain::AnvilChain, plaintext_peers: bool) -> Run {
        let decimals = anvil_chain::TOKEN_DECIMALS.to_string();
        let rpc_url = chain.rpc_url();
        let token = chain.token();
        let mut args = vec![
            "--clearnet",
            "toon.example.com",
            "--evm-rpc-url",
            &rpc_url,
            "--evm-token",
            &token,
            "--evm-decimals",
            &decimals,
        ];
        if plaintext_peers {
            args.push("--allow-plaintext-peers");
        }
        self.init_with(&args)
    }

    /// Run `toon init` for an agent node that settles in USDC on a local chain, paying
    /// into a channel with ERC-3009 as on Base.
    pub fn init_on_local(&self, chain: &local_chain::LocalChain) -> Run {
        let decimals = local_chain::TOKEN_DECIMALS.to_string();
        let token = chain.token();
        self.init_with(&[
            "--clearnet",
            "toon.example.com",
            "--evm-rpc-url",
            chain.rpc_url(),
            "--evm-token",
            &token,
            "--evm-decimals",
            &decimals,
        ])
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
