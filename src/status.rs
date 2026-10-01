//! `toon status` and `toon down`.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::control;
use crate::node::{self, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};

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
        lines.push(format!(
            "TOON app {}: connector {}{}.",
            app.name,
            if running { "running" } else { "not running" },
            field("address")
                .as_str()
                .map(|address| format!(" on {address}"))
                .unwrap_or_default()
        ));
        let apps: Vec<Value> = app
            .apps
            .iter()
            .map(|name| {
                let reported = reported
                    .and_then(|reported| reported["apps"].as_array())
                    .and_then(|apps| apps.iter().find(|app| app["name"] == name.as_str()));
                let field = |field: &str| reported.map_or(Value::Null, |app| app[field].clone());
                let running = field("running") == true;
                all_running &= running;
                lines.push(format!(
                    "App {name} of {}: {}{}.",
                    app.name,
                    if running { "running" } else { "not running" },
                    field("address")
                        .as_str()
                        .map(|address| format!(" on {address}"))
                        .unwrap_or_default()
                ));
                json!({ "name": name, "address": field("address"), "running": running })
            })
            .collect();
        toon_apps.push(json!({
            "name": app.name,
            "apps": apps,
            "connector": {
                "address": field("address"),
                "pid": field("pid"),
                "running": running,
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

/// Stop the supervisor and its connector, and return once both are gone. A node that
/// is not running is already down.
pub fn down(home: &Path) -> Result<Report, Error> {
    if State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let stopped = |was_running: bool, text: &str| Report {
        exit: Exit::Success,
        json: json!({ "stopped": was_running }),
        text: text.into(),
    };
    if control::ask(home, "down").is_none() {
        return Ok(stopped(false, "The agent node was not running."));
    }
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
    Ok(stopped(true, "The agent node stopped."))
}
