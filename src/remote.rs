//! `toon` with an agent key and no home (ADR 0010).
//!
//! `TOON_AGENT_KEY=<path>` or `--agent-key <path>` selects it. `toon` then reads the key
//! file and nothing else: no home, no keystore, no bearer token. A command goes out either as
//! an order, a request signed with the key, to the supervisor, or as a signed write (and
//! signed reads) to the operator surface of a connector the key file names. The connector
//! counts every payment against the key, so `toon` keeps no count of its own.
//!
//! Every command this module does not answer says `needs_home` before anything else is tried.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::cli::{AddArgs, Command, EventCommand, LimitCommand, MessageCommand, PeerCommand};
use crate::event;
use crate::message;
use crate::operator::{self, PeerAdd, Surface};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long an order waits for the supervisor.
const PATIENCE: Duration = Duration::from_secs(30);

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
        unanswered: None,
    }
}

/// An agent key file (`toon agent-key create` writes it).
struct KeyFile {
    secret: Zeroizing<[u8; 32]>,
    public: String,
    /// `http://host:port`
    supervisor: String,
    /// Each TOON app the key is granted on and its connector's operator surface, in the
    /// order the file gives them.
    toon_apps: Vec<(String, String)>,
}

impl KeyFile {
    fn load(path: &Path) -> Result<Self, Error> {
        let unusable = |reason: String| {
            failed(
                ErrorCode::Usage,
                format!("{} is not an agent key file: {reason}.", path.display()),
            )
        };
        let text =
            Zeroizing::new(std::fs::read_to_string(path).map_err(|source| {
                failed(ErrorCode::Io, format!("{}: {source}.", path.display()))
            })?);
        let file: Value =
            serde_json::from_str(&text).map_err(|error| unusable(error.to_string()))?;
        let secret: [u8; 32] = file["private_key"]
            .as_str()
            .and_then(|hex| hex::decode(hex).ok())
            .and_then(|raw| raw.try_into().ok())
            .ok_or_else(|| unusable("`private_key` is not 64 hex characters".into()))?;
        let public = file["public_key"]
            .as_str()
            .ok_or_else(|| unusable("it has no `public_key`".into()))?
            .to_ascii_lowercase();
        let supervisor = file["supervisor"]
            .as_str()
            .ok_or_else(|| unusable("it has no `supervisor`".into()))?
            .trim_end_matches('/')
            .to_owned();
        let toon_apps = file["toon_apps"]
            .as_object()
            .ok_or_else(|| unusable("it has no `toon_apps`".into()))?
            .iter()
            .map(|(name, url)| url.as_str().map(|url| (name.clone(), url.to_owned())))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unusable("an address in `toon_apps` is not text".into()))?;
        Ok(Self {
            secret: Zeroizing::new(secret),
            public,
            supervisor,
            toon_apps,
        })
    }

    fn keypair(&self) -> ed25519_dalek_v1::Keypair {
        operator::keypair_of(&self.secret[..]).expect("32 bytes are a secret key")
    }

    fn unknown_toon_app(&self, name: &str) -> Error {
        let known: Vec<&str> = self
            .toon_apps
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        failed(
            ErrorCode::UnknownName,
            format!(
                "This agent key file names no connector for the TOON app {name}. It names: {}.",
                known.join(", ")
            ),
        )
    }

    /// The operator surface of the TOON app `app`, or of the only one the file names, or of
    /// `relay`.
    fn surface(&self, app: Option<&str>) -> Result<Surface, Error> {
        let (name, url) = self.choose(app)?;
        Ok(Surface::of_agent_key(url, name, *self.secret))
    }

    /// The name and operator-surface address of the TOON app a command works on.
    fn choose(&self, app: Option<&str>) -> Result<&(String, String), Error> {
        Ok(match app {
            Some(name) => self
                .toon_apps
                .iter()
                .find(|(known, _)| known == name)
                .ok_or_else(|| self.unknown_toon_app(name))?,
            None => match self.toon_apps.as_slice() {
                [only] => only,
                many => many
                    .iter()
                    .find(|(name, _)| name == "relay")
                    .ok_or_else(|| {
                        failed(
                            ErrorCode::Usage,
                            "This agent key file names several TOON apps: say which with `--app`."
                                .into(),
                        )
                    })?,
            },
        })
    }
}

fn kebab(identifier: &str) -> String {
    let mut out = String::new();
    for (index, character) in identifier.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// The leading identifier of `text`.
fn leading(text: &str) -> &str {
    let end = text
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(text.len());
    &text[..end]
}

/// `wallet show`, `send`, `agent-key list`: the command as it is typed, from its `Debug`
/// form, which starts with the variant's name and, for a command with subcommands, holds
/// the subcommand's after `command: `.
fn name_of(command: &Command) -> String {
    let debug = format!("{command:?}");
    let variant = leading(&debug);
    let top = kebab(variant);
    // Only a command with subcommands has `command` as its first field.
    match debug[variant.len()..].strip_prefix(" { command: ") {
        Some(rest) => format!("{top} {}", kebab(leading(rest))),
        None => top,
    }
}

/// The refusal of a command that needs the home, before anything is tried.
fn needs_home(command: &Command) -> Error {
    let name = name_of(command);
    Error {
        nothing_sent: true,
        ..failed(
            ErrorCode::NeedsHome,
            format!(
                "`toon {name}` needs the agent node's home, which an agent key never reads. \
                 The operator must run it."
            ),
        )
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Send `body` as the order `name` to the supervisor, signed with the key, and return its
/// answer.
fn order(key: &KeyFile, name: &str, body: &Value) -> Result<Value, Error> {
    let path = format!("/orders/{name}");
    let url = format!("{}{path}", key.supervisor);
    let text = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let unreachable = |error: reqwest::Error| {
        failed(
            ErrorCode::NotRunning,
            format!(
                "The supervisor did not answer at {}: {error}. Is the agent node running, and \
                 is its loopback reachable from here?",
                key.supervisor
            ),
        )
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .map_err(unreachable)?;
    let keypair = key.keypair();
    let send = || -> Result<(u16, Value), Error> {
        let created = now();
        let (signature_input, signature, content_digest) =
            connector_operator::signing::sign_request(
                &keypair,
                "POST",
                &path,
                text.as_bytes(),
                created,
                Some(created + operator::SIGNATURE_TTL),
            );
        let mut request = client
            .post(&url)
            .header("content-digest", content_digest)
            .header("signature-input", signature_input)
            .header("signature", signature);
        if !text.is_empty() {
            request = request
                .header("content-type", "application/json")
                .body(text.clone());
        }
        let response = request.send().map_err(unreachable)?;
        let status = response.status().as_u16();
        let answer = response.json::<Value>().unwrap_or(Value::Null);
        Ok((status, answer))
    };
    let (mut status, mut answer) = send()?;
    // The same order twice in one second has the same signature, and is refused as a replay.
    // That refusal did nothing, so the order is signed again once the second has turned.
    if status == 401 && answer["error"]["code"] == "replayed" {
        operator::wait_for_next_second();
        (status, answer) = send()?;
    }
    if (200..300).contains(&status) {
        return Ok(answer);
    }
    let code = answer["error"]["code"].as_str().unwrap_or("refused");
    let message = answer["error"]["message"]
        .as_str()
        .unwrap_or("no reason given");
    Err(Error {
        nothing_sent: true,
        ..failed(
            ErrorCode::from_name(code).unwrap_or(ErrorCode::ConnectorFailed),
            format!("The supervisor refused the order with {status} ({code}): {message}"),
        )
    })
}

fn status(key: &KeyFile) -> Result<Report, Error> {
    let reply = order(key, "status", &Value::Null)?;
    let toon_apps = reply["toon_apps"].as_array().cloned().unwrap_or_default();
    let mut all_running = true;
    let mut lines = Vec::new();
    for app in &toon_apps {
        let connector = &app["connector"];
        let running = connector["running"].as_bool().unwrap_or(false);
        all_running &= running;
        lines.push(format!(
            "{}: connector {}{}.",
            app["name"].as_str().unwrap_or("?"),
            if running { "running" } else { "not running" },
            connector["address"]
                .as_str()
                .map(|address| format!(" at {address}"))
                .unwrap_or_default()
        ));
        for behind in app["apps"].as_array().into_iter().flatten() {
            let running = behind["running"].as_bool().unwrap_or(false);
            all_running &= running;
            lines.push(format!(
                "  {}: {}.",
                behind["name"].as_str().unwrap_or("?"),
                if running { "running" } else { "not running" }
            ));
        }
    }
    if toon_apps.is_empty() {
        lines.push("This key is granted on no TOON app that the supervisor runs.".into());
    }
    let pending_apps = reply["pending_apps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for app in &pending_apps {
        let what = match (app["image"].as_str(), app["url"].as_str()) {
            (Some(image), _) => format!("image {image}"),
            (_, Some(url)) => format!("the app at {url}"),
            _ => "an app".to_owned(),
        };
        let mut asked: Vec<String> = Vec::new();
        let allowances = &app["allowances"];
        if allowances["allow_egress"] == true {
            asked.push("egress".into());
        }
        for path in allowances["scratch"].as_array().into_iter().flatten() {
            asked.push(format!("scratch {}", path.as_str().unwrap_or_default()));
        }
        for limit in ["memory", "cpus", "pids"] {
            if let Some(value) = allowances[limit].as_str() {
                asked.push(format!("{limit} {value}"));
            }
        }
        for variable in app["variables"].as_array().into_iter().flatten() {
            asked.push(format!(
                "variable {}",
                variable.as_str().unwrap_or_default()
            ));
        }
        let with = if asked.is_empty() {
            String::new()
        } else {
            format!(", with {}", asked.join(", "))
        };
        let state = app["status"].as_str().unwrap_or("pending");
        let why = app["reason"]
            .as_str()
            .map(|reason| format!(": {reason}"))
            .unwrap_or_default();
        lines.push(format!(
            "{}: {state}{why} ({what} behind {}{with}).",
            app["name"].as_str().unwrap_or("?"),
            app["to"].as_str().unwrap_or("?")
        ));
    }
    Ok(Report {
        exit: if all_running {
            Exit::Success
        } else {
            Exit::Failure
        },
        json: json!({ "agent_key": key.public, "toon_apps": toon_apps, "pending_apps": pending_apps }),
        text: lines.join("\n"),
    })
}

fn logs(key: &KeyFile, name: &str, lines: usize) -> Result<Report, Error> {
    let reply = order(key, "logs", &json!({ "toon_app": name, "lines": lines }))?;
    let shown: Vec<&str> = reply["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "name": name,
            "toon_app": reply["toon_app"],
            "source": "connector",
            "lines": shown,
        }),
        text: shown.join("\n"),
    })
}

/// An order that changes the apps: its answer is what the command would have reported with
/// the home.
fn changing(key: &KeyFile, name: &str, body: &Value) -> Result<Report, Error> {
    let reply = order(key, name, body)?;
    Ok(Report {
        exit: Exit::Success,
        json: reply["result"].clone(),
        text: reply["text"].as_str().unwrap_or_default().to_owned(),
    })
}

fn add(key: &KeyFile, args: &AddArgs) -> Result<Report, Error> {
    // A secret is the operator's, and its file is never read here.
    if !args.secrets.secret.is_empty() || !args.secrets.secret_file.is_empty() {
        return Err(Error {
            nothing_sent: true,
            ..failed(
                ErrorCode::NeedsHome,
                "`toon add --secret` needs the agent node's home, which an agent key never \
                 reads. Only the operator supplies a secret."
                    .into(),
            )
        });
    }
    let request = match &args.request {
        Some(file) => Some(crate::apps::read_request(file)?),
        None => None,
    };
    changing(
        key,
        "add",
        &json!({
            "name": args.app,
            "to": args.to,
            "image": args.image,
            "url": args.url,
            "address": args.address,
            "price": args.price,
            "request": request,
            "allow_egress": args.allow.allow_egress,
            "scratch": args.allow.scratch,
            "memory": args.allow.memory,
            "cpus": args.allow.cpus,
            "pids": args.allow.pids,
            "env": args.secrets.env,
            "yes": args.yes,
        }),
    )
}

/// A command that moves money states its amount and says `--yes`, as it does with the home.
fn confirmed(amount: u128, yes: bool) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    Err(failed(
        ErrorCode::NotConfirmed,
        format!("This moves {amount} base units. Add `--yes` to say that you mean it."),
    ))
}

fn limit_show(key: &KeyFile, app: Option<&str>) -> Result<Report, Error> {
    let apps: Vec<&(String, String)> = match app {
        Some(name) => vec![key
            .toon_apps
            .iter()
            .find(|(known, _)| known == name)
            .ok_or_else(|| key.unknown_toon_app(name))?],
        None => key.toon_apps.iter().collect(),
    };
    let mut limits = serde_json::Map::new();
    let mut lines = Vec::new();
    for (name, url) in apps {
        let surface = Surface::of_agent_key(url, name, *key.secret);
        let listed = operator::write_keys(&surface)?;
        let mine = listed
            .iter()
            .find(|listed| {
                listed["keyid"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(&key.public))
            })
            .ok_or_else(|| {
                failed(
                    ErrorCode::ConnectorFailed,
                    format!("The connector of {name} does not list this agent key."),
                )
            })?;
        match &mine["spending_limit"] {
            Value::Null => {
                limits.insert(
                    name.clone(),
                    json!({ "per_command": null, "per_day": null, "remaining_today": null }),
                );
                lines.push(format!("{name}: this key states no spending limit."));
            }
            limit => {
                // A limit the key does not state is none, not zero.
                let figure = |field: &str| limit[field].as_u64();
                let (per_command, per_day) = (figure("per_write"), figure("per_day"));
                let remaining = per_day
                    .map(|per_day| per_day.saturating_sub(figure("counted_today").unwrap_or(0)));
                let shown =
                    |figure: Option<u64>| figure.map_or("none".to_owned(), |f| f.to_string());
                limits.insert(
                    name.clone(),
                    json!({
                        "per_command": per_command.map(|f| f.to_string()),
                        "per_day": per_day.map(|f| f.to_string()),
                        "remaining_today": remaining.map(|f| f.to_string()),
                    }),
                );
                lines.push(format!(
                    "{name}: per command {}, per day {}, remaining today {}.",
                    shown(per_command),
                    shown(per_day),
                    shown(remaining)
                ));
            }
        }
    }
    lines.push("Amounts are in the token's base units.".into());
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "agent_key": key.public, "limits": limits }),
        text: lines.join("\n"),
    })
}

/// `toon event sign` and `toon event publish` with an agent key: the supervisor signs with
/// the agent identity (the `sign` order) and the agent publishes, if it does, with the agent
/// key, which the connector counts. The secret is never here.
fn signed(
    key: &KeyFile,
    toon_app: Option<&str>,
    kind: u64,
    content: &str,
    tags: &str,
) -> Result<Value, Error> {
    let mut body = json!({
        "kind": kind,
        "content": content,
        "tags": event::parse_tags(tags)?,
    });
    if let Some(name) = toon_app {
        body["toon_app"] = json!(name);
    }
    order(key, "sign", &body)
}

fn event_sign(key: &KeyFile, kind: u64, content: &str, tags: &str) -> Result<Report, Error> {
    let event = signed(key, None, kind, content, tags)?["event"].clone();
    Ok(Report {
        exit: Exit::Success,
        text: event.to_string(),
        json: json!({ "event": event }),
    })
}

fn event_publish(
    key: &KeyFile,
    app: Option<&str>,
    (kind, content, tags): (u64, &str, &str),
    amount: u64,
) -> Result<Report, Error> {
    let (name, _) = key.choose(app)?;
    let reply = signed(key, Some(name), kind, content, tags)?;
    let event = reply["event"].clone();
    let relay = reply["relay"].as_str().ok_or_else(unexpected)?;
    let answer =
        operator::write_as_agent(&key.surface(app)?, relay, amount, event::write_body(&event))?;
    event::written(event, answer, amount)
}

fn unexpected() -> Error {
    failed(
        ErrorCode::ConnectorFailed,
        "The supervisor's answer was not understood.".into(),
    )
}

fn message_send(key: &KeyFile, app: Option<&str>, mut body: Value) -> Result<Report, Error> {
    let (name, _) = key.choose(app)?;
    body["toon_app"] = json!(name);
    let surface = key.surface(app)?;
    message::send_sealed(
        &body,
        |body| order(key, "seal", body),
        |wrap, relay| {
            let answer = operator::write_as_agent(&surface, relay, 0, event::write_body(wrap))?;
            event::written(wrap.clone(), answer, 0)
        },
    )
}

fn message_list(key: &KeyFile, body: &Value) -> Result<Report, Error> {
    let mut reply = order(key, "messages", body)?;
    let text = reply["text"].as_str().unwrap_or_default().to_owned();
    if let Some(object) = reply.as_object_mut() {
        object.remove("text");
    }
    Ok(Report {
        exit: Exit::Success,
        json: reply,
        text,
    })
}

/// Run `command` with the agent key at `path` and no home.
pub fn run(path: &Path, app: Option<&str>, command: Command) -> Result<Report, Error> {
    match command {
        Command::Status => status(&KeyFile::load(path)?),
        Command::Logs {
            name,
            connector: true,
            lines,
        } => logs(&KeyFile::load(path)?, &name, lines),
        Command::Logs { name, .. } => Err(failed(
            ErrorCode::Usage,
            format!(
                "An agent key reads only the connector's log of a TOON app: run `toon logs \
                 {name} --connector`."
            ),
        )),
        Command::Add(args) => add(&KeyFile::load(path)?, &args),
        Command::Remove { app, yes } => changing(
            &KeyFile::load(path)?,
            "remove",
            &json!({ "app": app, "yes": yes }),
        ),
        Command::Send(args) => {
            confirmed(args.amount.into(), args.yes)?;
            let surface = KeyFile::load(path)?.surface(app)?;
            let mut report = operator::send_as_agent(
                &surface,
                &args.address,
                args.amount,
                args.seal_to.as_deref(),
                (&args.method, &args.path),
                args.body.as_deref(),
            )?;
            if report.exit == Exit::Success {
                report.json["paid"] = json!(args.amount);
            }
            Ok(report)
        }
        Command::Peer {
            command: PeerCommand::Add(args),
        } => {
            confirmed(args.deposit, args.yes)?;
            let surface = KeyFile::load(path)?.surface(app)?;
            Ok(operator::peer_add_on(
                &surface,
                &PeerAdd {
                    address: &args.address,
                    deposit: args.deposit,
                    id: args.id.as_deref(),
                    fee: args.fee,
                    max_packet_amount: args.max_packet_amount,
                    chain: args.chain,
                },
            )?
            .report)
        }
        Command::Limit {
            command: LimitCommand::Show,
        } => limit_show(&KeyFile::load(path)?, app),
        Command::Event {
            command:
                EventCommand::Sign {
                    kind,
                    content,
                    tags,
                },
        } => event_sign(&KeyFile::load(path)?, kind, &content, &tags),
        Command::Event {
            command:
                EventCommand::Publish {
                    kind,
                    content,
                    tags,
                    amount,
                    relay: None,
                    yes: _,
                },
        } => event_publish(
            &KeyFile::load(path)?,
            app,
            (kind, &content, &tags),
            amount.unwrap_or(0),
        ),
        Command::Message {
            command:
                MessageCommand::Send {
                    recipients,
                    content,
                    reply_to,
                    subject,
                    relay: None,
                    yes: _,
                },
        } => message_send(
            &KeyFile::load(path)?,
            app,
            json!({
                "recipients": recipients,
                "content": content,
                "reply_to": reply_to,
                "subject": subject,
            }),
        ),
        Command::Message {
            command: MessageCommand::List { with, since, limit },
        } => message_list(
            &KeyFile::load(path)?,
            &json!({ "with": with, "since": since, "limit": limit }),
        ),
        other => Err(needs_home(&other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{AgentKeyCommand, WalletCommand};

    #[test]
    fn a_command_is_named_as_it_is_typed() {
        assert_eq!(name_of(&Command::Down), "down");
        assert_eq!(
            name_of(&Command::AgentKey {
                command: AgentKeyCommand::List
            }),
            "agent-key list"
        );
        assert_eq!(
            name_of(&Command::Wallet {
                command: WalletCommand::Show
            }),
            "wallet show"
        );
    }
}
