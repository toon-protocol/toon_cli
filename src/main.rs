//! `toon`: runs and manages an agent node.
//!
//! Every command is non-interactive. With `--json` a command prints exactly one JSON
//! document on standard output, whether it succeeded or failed; without it, it prints
//! readable text, and errors go to standard error. The exit codes are in `outcome` and
//! in `docs/exit-codes.md`.

mod cli;
mod home;
mod outcome;
mod status;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::error::ErrorKind;
use serde_json::json;

use cli::{Cli, Command};
use outcome::{Error, ErrorCode, Exit, Report};

fn main() -> ExitCode {
    let json = wants_json(env::args_os().skip(1));
    let outcome = match Cli::from_command_line() {
        Ok(cli) => run(&cli.command),
        Err(error) if json && error.kind() != ErrorKind::DisplayHelp => unparsed(error),
        // `--help`, or a command line that did not ask for JSON: the text is clap's own.
        Err(error) => {
            let exit = if error.use_stderr() {
                Exit::Usage
            } else {
                Exit::Success
            };
            return written(error.print(), exit).into();
        }
    };
    render(outcome, json).into()
}

fn run(command: &Command) -> Result<Report, Error> {
    match command {
        Command::Status => Ok(status::status(&home::resolve()?)),
    }
}

/// Whether the arguments ask for JSON. Asked of the raw arguments because a command
/// line that does not parse must still fail in the rendering it asked for.
fn wants_json(args: impl Iterator<Item = OsString>) -> bool {
    args.take_while(|arg| arg != "--")
        .any(|arg| arg == "--json" || arg.to_string_lossy().starts_with("--json="))
}

/// The JSON outcome of a command line clap did not turn into a command: `--version`,
/// or a usage error.
fn unparsed(error: clap::Error) -> Result<Report, Error> {
    if error.kind() == ErrorKind::DisplayVersion {
        return Ok(Report {
            exit: Exit::Success,
            json: json!({ "version": env!("CARGO_PKG_VERSION") }),
            text: String::new(),
        });
    }
    // clap renders "error: <what>", a blank line, then the usage and a hint.
    let rendered = error.render().to_string();
    let message = rendered
        .strip_prefix("error: ")
        .unwrap_or(&rendered)
        .lines()
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Err(Error {
        code: ErrorCode::Usage,
        message,
    })
}

fn render(outcome: Result<Report, Error>, json: bool) -> Exit {
    let mut stdout = io::stdout().lock();
    match outcome {
        Ok(report) if json => written(writeln!(stdout, "{}", report.json), report.exit),
        Ok(report) => written(writeln!(stdout, "{}", report.text), report.exit),
        Err(error) if json => written(writeln!(stdout, "{}", error.json()), error.code.exit()),
        Err(error) => written(
            writeln!(io::stderr(), "error: {}", error.message),
            error.code.exit(),
        ),
    }
}

/// `exit`, unless the output could not be written: a caller that got no output must
/// not read the exit code as the answer.
fn written(result: io::Result<()>, exit: Exit) -> Exit {
    match result {
        Ok(()) => exit,
        Err(_) => Exit::Failure,
    }
}
