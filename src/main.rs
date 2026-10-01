//! `toon`: runs and manages an agent node.
//!
//! Every command is non-interactive. With `--json` a command prints exactly one JSON
//! document on standard output, whether it succeeded or failed; without it, it prints
//! readable text, and errors go to standard error. The exit codes are in `outcome` and
//! in `docs/exit-codes.md`.

mod cli;
mod connector;
mod control;
mod derive;
mod home;
mod keystore;
mod node;
mod operator;
mod outcome;
mod status;
mod up;
mod wallet;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::error::ErrorKind;
use serde_json::json;

use cli::{ChannelCommand, Cli, Command, RouteCommand, WalletCommand};
use outcome::{Error, ErrorCode, Exit, Report};
use up::Stopped;

fn main() -> ExitCode {
    let json = wants_json(env::args_os().skip(1));
    let command = match Cli::from_command_line() {
        Ok(cli) => cli.command,
        Err(error) if json && error.kind() != ErrorKind::DisplayHelp => {
            return render(unparsed(error), json).into()
        }
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
    match command {
        Command::Status => {
            render(home::resolve().and_then(|home| status::status(&home)), json).into()
        }
        Command::Down => render(home::resolve().and_then(|home| status::down(&home)), json).into(),
        Command::Init(args) => render(
            home::resolve().and_then(|home| wallet::init(&home, &args.options())),
            json,
        )
        .into(),
        Command::Wallet {
            command: WalletCommand::Show,
        } => render(home::resolve().and_then(|home| wallet::show(&home)), json).into(),
        Command::Wallet {
            command: WalletCommand::Balances,
        } => render(
            home::resolve().and_then(|home| wallet::balances(&home)),
            json,
        )
        .into(),
        Command::Channel { command } => render(
            home::resolve().and_then(|home| match command {
                ChannelCommand::List => operator::channel_list(&home),
                ChannelCommand::Open {
                    terms,
                    deposit,
                    url,
                } => operator::channel_open(&home, &terms, deposit, url.as_deref()),
                ChannelCommand::Fund { id, amount } => operator::channel_fund(&home, &id, amount),
                ChannelCommand::Withdraw { id } => operator::channel_withdraw(&home, &id),
                ChannelCommand::Land { id } => operator::channel_land(&home, &id),
            }),
            json,
        )
        .into(),
        Command::Send(args) => render(
            home::resolve().and_then(|home| operator::send(&home, &args.address, args.amount)),
            json,
        )
        .into(),
        Command::Route {
            command: RouteCommand::List,
        } => render(
            home::resolve().and_then(|home| operator::route_list(&home)),
            json,
        )
        .into(),
        Command::Up => up(json).into(),
        // The connector this binary embeds, as the supervisor's child: it reports to
        // the supervisor and not to an operator.
        Command::Connector { config } => connector::serve(&config),
    }
}

/// `toon up` reports once its connector is listening and then stays in the foreground,
/// so it renders twice: the report, and why it stopped.
fn up(json: bool) -> Exit {
    let supervisor = match home::resolve().and_then(|home| up::start(&home)) {
        Ok(supervisor) => supervisor,
        Err(error) => return render(Err(error), json),
    };
    match render(Ok(supervisor.report()), json) {
        Exit::Success => {}
        unwritten => return unwritten,
    }
    match supervisor.wait() {
        Stopped::Down => Exit::Success,
        // The one JSON document has been printed; the exit code is all that is left.
        Stopped::Failed(error) if json => error.code.exit(),
        Stopped::Failed(error) => render(Err(error), json),
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
            json: json!({
                "version": env!("CARGO_PKG_VERSION"),
                "connector_revision": connector::REVISION,
                "relay_image": env!("TOON_RELAY_IMAGE"),
            }),
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
