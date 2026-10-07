//! `toon status` and `toon down`.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control;
use crate::node::{self, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::service;

/// How long `down` waits for the supervisor to finish stopping.
const STOPPING: Duration = Duration::from_secs(30);

/// The agent node at `home`: no agent node until `toon init` has recorded a TOON app,
/// and then what the supervisor says is running. Anything down exits 1, and the report
/// is printed all the same.
pub fn status(home: &Path) -> Result<Report, Error> {
    let shown = home.to_string_lossy();
    let Some(state) = State::load(home)? else {
        return Ok(Report {
            exit: Exit::NoAgentNode,
            json: json!({ "home": shown, "agent_node": null }),
            text: format!("No agent node at {shown}."),
        });
    };
    let reply = control::ask(home, "status");
    let supervisor_running = reply.is_some();
    let mut all_running = supervisor_running;
    let mut lines = vec![format!(
        "Supervisor: {}.",
        if supervisor_running {
            "running"
        } else {
            "not running"
        }
    )];
    // Asked of each running connector, as `toon peer list` does: unknown (`None`) as soon as
    // one does not answer. A peering made with `peer add` does not set `joined`.
    let peerings: Option<Vec<(&str, String)>> = state
        .toon_apps
        .iter()
        .map(|app| {
            crate::operator::peering_ids(home, &app.name).map(|ids| {
                ids.into_iter()
                    .map(|id| (app.name.as_str(), id))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|all| all.into_iter().flatten().collect());
    match (&state.joined, &peerings) {
        (Some(network), _) => lines.push(format!(
            "Connected to {network}. Reading {}.",
            state.reads.join(", ")
        )),
        (None, None) => lines.push(
            "No network joined. Peerings are unknown while the connector is not running.".into(),
        ),
        (None, Some(held)) if !held.is_empty() => {
            let ids: Vec<&str> = held.iter().map(|(_, id)| id.as_str()).collect();
            lines.push(format!(
                "No network joined. {} {}: {}.",
                ids.len(),
                if ids.len() == 1 { "peering" } else { "peerings" },
                ids.join(", ")
            ));
        }
        (None, Some(_)) => lines.push(
            "Unconnected: it has joined no network. `toon join <network> --deposit <amount> --yes` connects it."
                .into(),
        ),
    }
    let mut toon_apps = Vec::new();
    for app in &state.toon_apps {
        let reported = reply
            .as_ref()
            .and_then(|reply| reply["toon_apps"].as_array())
            .and_then(|apps| {
                apps.iter()
                    .find(|reported| reported["name"] == app.name.as_str())
            });
        let connector = reported.map(|reported| &reported["connector"]);
        let field = |name: &str| connector.map_or(Value::Null, |connector| connector[name].clone());
        let running = field("running") == true;
        all_running &= running;
        let restarts = field("restarts").as_u64().unwrap_or(0);
        lines.push(format!(
            "TOON app {} (ILP address {}): connector {}{}{}.",
            app.name,
            app.address(),
            if running { "running" } else { "not running" },
            field("address")
                .as_str()
                .map(|address| format!(" on {address}"))
                .unwrap_or_default(),
            match restarts {
                0 => String::new(),
                1 => ", restarted once".to_string(),
                times => format!(", restarted {times} times"),
            }
        ));
        if let Some(why) = field("last_exit").as_str() {
            lines.push(format!("  Last stopped: {why}"));
        }
        let apps: Vec<Value> = app
            .apps
            .iter()
            .map(|behind| {
                let name = &behind.name;
                let reported = reported
                    .and_then(|reported| reported["apps"].as_array())
                    .and_then(|apps| apps.iter().find(|app| app["name"] == name.as_str()));
                let field = |field: &str| reported.map_or(Value::Null, |app| app[field].clone());
                let route = format!("Route {} at price {}.", behind.prefix, behind.price);
                // An app the operator serves is not the supervisor's to run, so it is
                // neither running nor stopped as far as `status` can say.
                if let node::Source::Url(url) = &behind.source {
                    lines.push(format!(
                        "App {name} of {}: served at {url}. {route}",
                        app.name
                    ));
                    return json!({
                        "name": name, "url": url, "running": null,
                        "prefix": behind.prefix, "price": behind.price,
                    });
                }
                let running = field("running") == true;
                all_running &= running;
                lines.push(format!(
                    "App {name} of {}: {}{}.{} {route}",
                    app.name,
                    if running { "running" } else { "not running" },
                    field("address")
                        .as_str()
                        .map(|address| format!(" on {address}"))
                        .unwrap_or_default(),
                    field("read_address")
                        .as_str()
                        .map(|read| format!(" Read at ws://{read}."))
                        .unwrap_or_default()
                ));
                let image = match &behind.source {
                    node::Source::Image(image) => json!(image),
                    _ => Value::Null,
                };
                json!({
                    "name": name, "address": field("address"),
                    "read_address": field("read_address"), "running": running,
                    "image": image, "prefix": behind.prefix, "price": behind.price,
                })
            })
            .collect();
        toon_apps.push(json!({
            "name": app.name,
            "apps": apps,
            "connector": {
                "address": field("address"),
                "ilp_address": app.address(),
                "pid": field("pid"),
                "running": running,
                "restarts": field("restarts"),
                "last_exit": field("last_exit"),
            },
        }));
    }
    // What `subscribe` and the supervisor last kept, not what each relay says now: asking
    // them is `toon relay subscriptions`.
    let mut subscriptions = Vec::new();
    let all_kept = crate::subscribe::load(home)?;
    let (active, exhausted_count) = crate::subscribe::totals(&all_kept);
    for kept in all_kept {
        let exhausted = kept.exhausted();
        lines.push(if exhausted {
            format!(
                "Subscription at {}: exhausted. `toon relay subscribe` tops it up.",
                kept.relay
            )
        } else {
            format!(
                "Subscription at {}: balance {}, as last read {}. `toon relay subscriptions` \
                 reads the current one.",
                kept.relay,
                kept.balance,
                kept.read_at.map_or_else(
                    || "at an unknown time".to_owned(),
                    |at| format!("at {at} (Unix seconds)")
                )
            )
        });
        subscriptions.push(json!({
            "relay": kept.relay,
            "balance": kept.balance,
            "read_at": kept.read_at,
            "broadcast_price": kept.broadcast_price,
            "exhausted": exhausted,
        }));
    }
    // Asked of the running relay: unknown while it does not answer, which is not a failure.
    let subscribers = crate::subscribe::incoming_with_balance(home, &state);
    lines.push(format!(
        "Subscriptions held: {active} with a balance, {exhausted_count} exhausted."
    ));
    lines.push(format!(
        "Subscriber keys of its own relay with a balance: {}.",
        subscribers.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    ));
    Ok(Report {
        exit: if all_running {
            Exit::Success
        } else {
            Exit::Failure
        },
        json: json!({
            "home": shown,
            "agent_node": {
                "supervisor": { "running": supervisor_running, "socket": control::path(home) },
                "joined": state.joined,
                "peerings": peerings.map(|held| held
                    .into_iter()
                    .map(|(toon_app, id)| json!({ "toon_app": toon_app, "id": id }))
                    .collect::<Vec<_>>()),
                "reads": state.reads,
                "toon_apps": toon_apps,
                "subscriptions": subscriptions,
                "totals": {
                    "subscriptions": { "active": active, "exhausted": exhausted_count },
                    "subscribers": subscribers,
                },
            },
        }),
        text: lines.join("\n"),
    })
}

/// Stop the supervisor and its connector, and return once both are gone, and stop the
/// `systemd --user` unit that `up` installed, so that it does not start them again. A
/// node that is not running is already down.
pub fn down(home: &Path) -> Result<Report, Error> {
    if State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let stopped = |was_running: bool, text: &str| Report {
        exit: Exit::Success,
        json: json!({ "stopped": was_running }),
        text: text.into(),
    };
    let was_running = control::ask(home, "down").is_some();
    if was_running {
        // The supervisor removes its socket last, when the connector has exited. One that
        // dies on the way leaves its socket behind, but nothing answers on it.
        let deadline = Instant::now() + STOPPING;
        while control::running(home) {
            if Instant::now() >= deadline {
                return Err(Error {
                    nothing_sent: false,
                    unanswered: None,
                    code: ErrorCode::ConnectorFailed,
                    message: "The supervisor was asked to stop and had not stopped in time.".into(),
                });
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    service::remove()?;
    Ok(if was_running {
        stopped(true, "The agent node stopped.")
    } else {
        stopped(false, "The agent node was not running.")
    })
}

/// How many lines of a log `toon logs` shows when it is not told.
pub const DEFAULT_LINES: usize = 100;

/// The last `lines` lines of the log of the TOON app or app called `name`. An app's
/// requests pass through the connector of its TOON app, so until an app runs as a
/// process of its own, the connector's log is the app's log.
pub fn logs(home: &Path, name: &str, lines: usize) -> Result<Report, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let Some(app) = state
        .toon_apps
        .iter()
        .find(|app| app.name == name || app.apps.iter().any(|behind| behind.name == name))
    else {
        let known: Vec<&str> = state
            .toon_apps
            .iter()
            .flat_map(|app| {
                std::iter::once(&app.name).chain(app.apps.iter().map(|behind| &behind.name))
            })
            .map(String::as_str)
            .collect();
        return Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::UnknownName,
            message: format!(
                "No TOON app or app is called {name}. This agent node has {}.",
                known.join(", ")
            ),
        });
    };
    let log = node::ConnectorFiles::of(home, app.connector).log;
    let text = read_log(&log)?;
    let all: Vec<&str> = text.lines().collect();
    let shown = &all[all.len().saturating_sub(lines)..];
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "name": name, "toon_app": app.name, "log": log, "lines": shown }),
        text: shown.join("\n"),
    })
}

/// The text of a connector's log. A TOON app that has never started has no log yet, which
/// reads as empty.
pub fn read_log(log: &Path) -> Result<String, Error> {
    match std::fs::read(log) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(Error {
            nothing_sent: false,
            unanswered: None,
            code: ErrorCode::Io,
            message: format!("{}: {source}.", log.display()),
        }),
    }
}

/// How many rejected packets `toon packet list` shows when it is not told.
pub const DEFAULT_PACKETS: usize = 20;

/// A packet the connector rejected, as its log line says.
#[derive(Debug, PartialEq, Eq)]
pub struct Rejected {
    pub time: String,
    pub destination: String,
    pub code: String,
    pub message: String,
}

/// The packet the line says was rejected, or `None` for any other line, one that is not JSON
/// and a reject that lacks a field. The connector's `packet rejected` line carries `message`
/// twice, the event's first and the reject's after the `code`; a JSON map keeps the last, the
/// reject's, so the line is recognised by its text instead, where a reject's own text cannot
/// imitate the event's: its quotes are escaped. A line with no second `message` lacks the
/// reject's, and the map would hand back the event's in its place.
fn rejected(line: &str) -> Option<Rejected> {
    let (_, after) = line.split_once(r#""fields":{"message":"packet rejected","#)?;
    if !after.contains(r#""message":"#) {
        return None;
    }
    let line: Value = serde_json::from_str(line).ok()?;
    let field = |value: &Value, key: &str| value.get(key)?.as_str().map(str::to_owned);
    let fields = line.get("fields")?;
    Some(Rejected {
        time: field(&line, "timestamp")?,
        destination: field(line.get("span")?, "destination")?,
        code: field(fields, "code")?,
        message: field(fields, "message")?,
    })
}

/// The rejected packets in a connector's log, newest first, at most `limit`.
pub fn rejected_packets(log: &str, limit: usize) -> Vec<Rejected> {
    log.lines().rev().filter_map(rejected).take(limit).collect()
}

#[cfg(test)]
mod rejected_tests {
    use super::*;

    const REJECT: &str = r#"{"timestamp":"2026-10-03T23:00:30.314592Z","level":"INFO","fields":{"message":"packet rejected","code":"F02","message":"no route"},"target":"c","span":{"correlation_id":"x","destination":"g.a","name":"packet"},"spans":[]}"#;

    #[test]
    fn a_reject_among_other_lines_is_read() {
        let log = format!(
            "{}\nnot json\n{{\"other\":1}}\n{REJECT}\n",
            r#"{"timestamp":"t","fields":{"message":"connector listening"}}"#
        );
        assert_eq!(
            rejected_packets(&log, 20),
            vec![Rejected {
                time: "2026-10-03T23:00:30.314592Z".into(),
                destination: "g.a".into(),
                code: "F02".into(),
                message: "no route".into(),
            }]
        );
    }

    #[test]
    fn a_reject_missing_a_field_is_skipped() {
        let no_code = REJECT.replace(r#""code":"F02","#, "");
        let no_destination = REJECT.replace(r#""destination":"g.a","#, "");
        let no_message = REJECT.replace(r#","message":"no route""#, "");
        let no_time = REJECT.replace(r#""timestamp":"2026-10-03T23:00:30.314592Z","#, "");
        let log = [no_code, no_destination, no_message, no_time].join("\n");
        assert!(rejected_packets(&log, 20).is_empty());
    }

    #[test]
    fn an_empty_log_has_no_rejects() {
        assert!(rejected_packets("", 20).is_empty());
    }

    #[test]
    fn newest_first_and_limited() {
        let later = REJECT.replace("g.a", "g.b");
        let log = format!("{REJECT}\n{later}\n");
        let all = rejected_packets(&log, 20);
        assert_eq!(all[0].destination, "g.b");
        assert_eq!(rejected_packets(&log, 1).len(), 1);
    }
}
