//! `toon`: runs and manages an agent node.
//!
//! Every command is non-interactive. With `--json` a command prints exactly one JSON
//! document on standard output, whether it succeeded or failed; without it, it prints
//! readable text, and errors go to standard error. The exit codes are in `outcome` and
//! in `docs/exit-codes.md`.

mod anon;
mod apps;
mod cli;
mod connector;
mod control;
mod derive;
mod egress;
mod event;
mod feed;
mod funding;
mod home;
mod keystore;
mod lifecycle;
mod nip;
mod node;
mod operator;
mod outcome;
mod overlay;
mod ports;
mod profile;
mod receive;
mod relay;
mod relay_url;
mod runner;
mod service;
mod skill;
mod spending;
mod status;
mod subscribe;
mod tls;
mod up;
mod wallet;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::error::ErrorKind;
use serde_json::json;

use cli::{Cli, Command, LimitCommand};
use outcome::{Error, ErrorCode, Exit, Report};
use up::Stopped;

fn main() -> ExitCode {
    let code = run();
    egress::release();
    code
}

fn run() -> ExitCode {
    let json = wants_json(env::args_os().skip(1));
    let command = match Cli::from_command_line() {
        Ok(cli) => {
            operator::target(cli.app.as_deref());
            (cli.app, cli.command)
        }
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
    let (app, command) = command;
    match command {
        Command::Status => {
            render(home::resolve().and_then(|home| status::status(&home)), json).into()
        }
        Command::Down => render(home::resolve().and_then(|home| status::down(&home)), json).into(),
        Command::Init(args) => render(
            home::resolve()
                .and_then(|home| wallet::init(&home, &args.options(), args.from_mnemonic)),
            json,
        )
        .into(),
        Command::Wallet { command } => render(
            home::resolve().and_then(|home| wallet::run(&home, &command)),
            json,
        )
        .into(),
        Command::Channel { command } => render(
            home::resolve().and_then(|home| operator::channel(&home, command)),
            json,
        )
        .into(),
        Command::Send(args) => render(
            home::resolve().and_then(|home| {
                let amount: u128 = args.amount.into();
                spending::spend_packets(&home, amount, args.yes, |packets| {
                    let mut report =
                        operator::send(&home, &args.address, args.amount, args.seal_to.as_deref())?;
                    // A packet that was rejected, or fulfilled wrongly, moved what its
                    // channels moved by, which is nothing when the agent node's own
                    // connector refused it.
                    let paid = if report.exit == Exit::Success {
                        amount
                    } else {
                        packets.moved(amount)
                    };
                    report.json["paid"] = json!(paid);
                    if report.exit != Exit::Success && paid > 0 {
                        report.text = format!("{} It cost {paid} base units.", report.text);
                    }
                    Ok((report, paid))
                })
            }),
            json,
        )
        .into(),
        Command::Add(args) => render(
            home::resolve().and_then(|home| {
                apps::add(
                    &home,
                    &apps::Add {
                        name: &args.app,
                        to: &args.to,
                        origin: match (&args.image, &args.url) {
                            (_, Some(url)) => apps::Origin::Url(url),
                            (Some(image), None) => apps::Origin::Image(image),
                            (None, None) => unreachable!("clap requires one of them"),
                        },
                        address: args.address.as_deref(),
                        price: args.price,
                        yes: args.yes,
                    },
                )
            }),
            json,
        )
        .into(),
        Command::Create(args) => render(
            home::resolve().and_then(|home| {
                lifecycle::create(
                    &home,
                    &lifecycle::Create {
                        name: &args.name,
                        from: app.as_deref(),
                        origin: match (&args.image, &args.url) {
                            (_, Some(url)) => apps::Origin::Url(url),
                            (Some(image), None) => apps::Origin::Image(image),
                            (None, None) => unreachable!("clap requires one of them"),
                        },
                        price: args.price,
                        reach: match &args.clearnet {
                            Some(hostname) => node::Reach::Clearnet {
                                hostname: hostname.clone(),
                            },
                            None => node::Reach::Hidden,
                        },
                        accept_anyone_terms: args.accept_anyone_terms,
                        listen: &args.listen,
                        deposit: args.deposit,
                        yes: args.yes,
                    },
                )
            }),
            json,
        )
        .into(),
        Command::Destroy { name } => render(
            home::resolve().and_then(|home| lifecycle::destroy(&home, &name)),
            json,
        )
        .into(),
        Command::Remove { app, yes } => render(
            home::resolve().and_then(|home| apps::remove(&home, &app, yes)),
            json,
        )
        .into(),
        Command::Peer { command } => render(
            home::resolve().and_then(|home| operator::peer(&home, &command)),
            json,
        )
        .into(),
        Command::Join(args) => render(
            home::resolve().and_then(|home| operator::join(&home, &args)),
            json,
        )
        .into(),
        Command::Limit { command } => render(
            home::resolve().and_then(|home| match command {
                LimitCommand::Show => spending::show(&home),
                LimitCommand::Set {
                    max_per_command,
                    max_per_day,
                } => spending::set(&home, max_per_command, max_per_day),
            }),
            json,
        )
        .into(),
        Command::Route { command } => render(
            home::resolve().and_then(|home| operator::route(&home, &command)),
            json,
        )
        .into(),
        Command::Event { command } => render(event::run(command), json).into(),
        Command::Nip { command } => render(nip::run(command), json).into(),
        Command::Skill { command } => render(skill::run(command), json).into(),
        Command::Relay { command } => render(
            home::resolve().and_then(|home| relay::run(&home, command)),
            json,
        )
        .into(),
        Command::Up { foreground: true } => up(json).into(),
        Command::Up { foreground: false } => {
            render(home::resolve().and_then(|home| install(&home)), json).into()
        }
        Command::Logs { name, lines } => render(
            home::resolve().and_then(|home| status::logs(&home, &name, lines)),
            json,
        )
        .into(),
        // The connector this binary embeds, as the supervisor's child: it reports to
        // the supervisor and not to an operator.
        Command::Connector { config } => connector::serve(&config),
    }
}

/// `toon up --foreground` reports once its connector is listening and then stays in the foreground,
/// so it renders once, and its exit code says whether `toon down` stopped it or an app did.
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

/// `toon up` without `--foreground`: install the unit that runs the supervisor, and
/// leave it running.
fn install(home: &Path) -> Result<Report, Error> {
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    if control::running(home) {
        return Err(Error {
            nothing_sent: false,
            code: ErrorCode::AlreadyRunning,
            message: format!(
                "A supervisor is already running this agent node, at {}.",
                control::path(home).display()
            ),
        });
    }
    let installed = service::install()?;
    let unit = installed.unit.to_string_lossy();
    let linger = if installed.linger {
        ""
    } else {
        " It will start at your first login, not at boot: run `loginctl enable-linger` to change that."
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "home": home,
            "unit": { "name": service::UNIT, "path": unit },
            "linger": installed.linger,
        }),
        text: format!(
            "Started {} ({unit}). `toon status` shows what it runs.{linger}",
            service::UNIT
        ),
    })
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
        nothing_sent: false,
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
