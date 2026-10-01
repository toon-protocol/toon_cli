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
            "TOON app {}: connector {}{}{}.",
            app.name,
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
                    "App {name} of {}: {}{}. {route}",
                    app.name,
                    if running { "running" } else { "not running" },
                    field("address")
                        .as_str()
                        .map(|address| format!(" on {address}"))
                        .unwrap_or_default()
                ));
                let image = match &behind.source {
                    node::Source::Image(image) => json!(image),
                    _ => Value::Null,
                };
                json!({
                    "name": name, "address": field("address"), "running": running,
                    "image": image, "prefix": behind.prefix, "price": behind.price,
                })
            })
            .collect();
        toon_apps.push(json!({
            "name": app.name,
            "apps": apps,
            "connector": {
                "address": field("address"),
                "pid": field("pid"),
                "running": running,
                "restarts": field("restarts"),
                "last_exit": field("last_exit"),
            },
        }));
    }
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
                "toon_apps": toon_apps,
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
            code: ErrorCode::UnknownName,
            message: format!(
                "No TOON app or app is called {name}. This agent node has {}.",
                known.join(", ")
            ),
        });
    };
    let log = node::ConnectorFiles::of(home, app.connector).log;
    let text = match std::fs::read(&log) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // A TOON app that has never started has no log yet.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error {
                code: ErrorCode::Io,
                message: format!("{}: {source}.", log.display()),
            })
        }
    };
    let all: Vec<&str> = text.lines().collect();
    let shown = &all[all.len().saturating_sub(lines)..];
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "name": name, "toon_app": app.name, "log": log, "lines": shown }),
        text: shown.join("\n"),
    })
}
