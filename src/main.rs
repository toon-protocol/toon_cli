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
use clap::Parser;
use serde_json::json;

use cli::{Cli, Command};
use outcome::{Error, Exit, Report};

fn main() -> ExitCode {
    let json = wants_json(env::args_os().skip(1));
    let outcome = match Cli::try_parse() {
        Ok(cli) => run(&cli.command),
        Err(error) => match unparsed(error, json) {
            Ok(outcome) => outcome,
            Err(exit) => return exit.into(),
        },
    };
    render(outcome, json).into()
}

fn run(command: &Command) -> Result<Report, Error> {
    match command {
        Command::Status => status::status(&home::resolve()?),
    }
}

/// Whether the arguments ask for JSON. Asked of the raw arguments because a command
/// line that does not parse must still fail in the rendering it asked for.
fn wants_json(args: impl Iterator<Item = OsString>) -> bool {
    args.take_while(|arg| arg != "--")
        .any(|arg| arg == "--json")
}

/// What to do with a command line clap did not turn into a command: `--help`,
/// `--version`, or a usage error. Text is clap's own and is printed here, leaving only
/// the exit code; JSON comes back as an outcome to render like any other.
fn unparsed(error: clap::Error, json: bool) -> Result<Result<Report, Error>, Exit> {
    let exit = if error.use_stderr() {
        Exit::Usage
    } else {
        Exit::Success
    };
    if !json || error.kind() == ErrorKind::DisplayHelp {
        // Nothing useful can be done if the terminal is gone.
        let _ = error.print();
        return Err(exit);
    }
    if error.kind() == ErrorKind::DisplayVersion {
        return Ok(Ok(Report {
            exit,
            json: json!({ "version": env!("CARGO_PKG_VERSION") }),
            text: String::new(),
        }));
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
    Ok(Err(Error {
        exit: Exit::Usage,
        code: "usage",
        message,
    }))
}

fn render(outcome: Result<Report, Error>, json: bool) -> Exit {
    // A closed pipe is not worth a panic: the exit code still tells the caller.
    let mut stdout = io::stdout().lock();
    match outcome {
        Ok(report) => {
            let _ = if json {
                writeln!(stdout, "{}", report.json)
            } else {
                writeln!(stdout, "{}", report.text)
            };
            report.exit
        }
        Err(error) => {
            let _ = if json {
                writeln!(stdout, "{}", error.json())
            } else {
                writeln!(io::stderr(), "error: {}", error.message)
            };
            error.exit
        }
    }
}
