//! `toon tip enable` and `toon tip disable`: whether the agent node can be tipped (ADR 0013).
//!
//! A tip route is a route of one connector that is paid whatever arrives. The supervisor is
//! the app behind it (`acknowledge`). Enabling records the route in the state, restarts the
//! connector that serves it and publishes the tip route (kind 10850) to the agent node's own
//! relay and to the relay its profile follows; disabling takes the route away and publishes a
//! NIP-09 deletion of that event to both.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::acknowledge;
use crate::apps;
use crate::cli::TipCommand;
use crate::event;
use crate::inbox;
use crate::node::{self, Reach, State, Tip, ToonApp};
use crate::operator;
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::ports;
use crate::subscribe;

/// The kind of the event that names a tip route: replaceable, one per agent identity.
pub const ROUTE: u64 = 10_850;

/// The kind of NIP-09's deletion request.
const DELETION: u64 = 5;

/// How long the connector gets to answer a question about its identity.
const PATIENCE: Duration = Duration::from_secs(10);

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code,
        message,
    }
}

/// `toon tip`.
pub fn run(home: &Path, app: Option<&str>, command: &TipCommand) -> Result<Report, Error> {
    match command {
        TipCommand::Enable { minimum, yes } => enable(home, app, *minimum, *yes),
        TipCommand::Disable { yes } => disable(home, *yes),
    }
}

/// The TOON app that has the tip route, if one does.
fn serving(state: &State) -> Option<&ToonApp> {
    state.toon_apps.iter().find(|app| app.tip.is_some())
}

fn not_running() -> Error {
    failed(
        ErrorCode::NotRunning,
        "The agent node is not running. Run `toon up`.".into(),
    )
}

/// Where the connector of `app` is reached by the clients that tip: the URL its operator
/// publishes, the onion endpoint of a hidden service or the public hostname.
fn connector_url(home: &Path, app: &ToonApp) -> Result<String, Error> {
    match &app.reach {
        Reach::Clearnet { hostname } => Ok(format!("https://{hostname}/ilp")),
        Reach::Hidden => node::onion_endpoint(home, app)
            .map(|endpoint| format!("http://{endpoint}/ilp"))
            .ok_or_else(|| {
                failed(
                    ErrorCode::OverlayUnavailable,
                    format!("The onion endpoint of {} is not known yet.", app.name),
                )
            }),
    }
}

/// The sealing key the running connector of `app` reports on its `/identity`, as it reports it.
fn seal_key(home: &Path, app: &ToonApp) -> Result<String, Error> {
    let surface = operator::surface_of(home, Some(&app.name))?;
    let url = format!("{}/ilp/identity", surface.url);
    let identity: Value = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .and_then(|client| client.get(&url).send())
        .and_then(|response| response.json())
        .map_err(|error| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{url} did not give its identity: {error}."),
            )
        })?;
    identity["publicKey"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{url} has no `publicKey`."),
            )
        })
}

/// What a published event came to, as an error if the relay did not take it.
fn taken(report: &Report) -> Result<(), String> {
    if report.exit == Exit::Success {
        Ok(())
    } else {
        Err(report.text.clone())
    }
}

/// Publish the signed `event` to the agent node's own relay and to the relay the profile
/// follows, which is paid for. Returns where each went, or what stopped it.
fn publish(home: &Path, relay: &str, event: &Value) -> Result<Value, Error> {
    let own = event::write(home, event.clone(), 0)?;
    taken(&own).map_err(|why| {
        failed(
            ErrorCode::SendFailed,
            format!("The agent node's own relay did not take the event: {why}"),
        )
    })?;
    let followed = event::publish_signed_to(home, relay, event.clone())?;
    taken(&followed).map_err(|why| {
        failed(
            ErrorCode::SendFailed,
            format!("{relay} did not take the event: {why}"),
        )
    })?;
    Ok(json!({
        "id": event["id"],
        "own_relay": true,
        "followed_relay": relay,
        "paid": followed.json["paid"],
    }))
}

/// The relay the agent node's profile follows, which the tip route and its acknowledgements
/// are published to as well as to the agent node's own.
fn profile_relay(state: &State) -> Result<String, Error> {
    state.relay_url.clone().ok_or_else(|| {
        failed(
            ErrorCode::NoProfileRelay,
            format!(
                "The network profile {} follows no relay, so there is nowhere but this agent \
                 node's own relay to publish a tip route to, and nobody would find it.",
                state.network.name()
            ),
        )
    })
}

fn enable(home: &Path, app: Option<&str>, minimum: u64, yes: bool) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let target = match app {
        Some(name) => apps::toon_app(&state, Some(name))?,
        None => subscribe::own_relay_app(&state)?,
    }
    .clone();
    let relay = profile_relay(&state)?;
    let secret = inbox::require_kept_secret(home)?;
    // The route is served and published only while the connector runs.
    operator::surface_of(home, Some(&target.name)).map_err(|_| not_running())?;
    apps::confirm(yes, &target.name)?;

    let mut changed = state.clone();
    for toon_app in &mut changed.toon_apps {
        // One tip route to an agent node: asking for another TOON app moves it.
        if toon_app.name != target.name {
            toon_app.tip = None;
        }
    }
    let port = match target.tip {
        Some(tip) => tip.port,
        None => ports::kept("127.0.0.1").map_err(|source| {
            failed(
                ErrorCode::Io,
                format!("A port for the tip route could not be picked: {source}."),
            )
        })?,
    };
    let route = Tip { minimum, port };
    for toon_app in &mut changed.toon_apps {
        if toon_app.name == target.name {
            toon_app.tip = Some(route);
        }
    }
    // The other TOON app, if the route moves, stops serving it too.
    let moved_from = serving(&state)
        .filter(|before| before.name != target.name)
        .map(|before| before.name.clone());
    let restarted = apps::apply(home, &state, &changed, &target.name, || {})?;
    if let Some(from) = &moved_from {
        apps::apply(home, &changed, &changed, from, || {})?;
    }

    let target = changed
        .toon_apps
        .iter()
        .find(|toon_app| toon_app.name == target.name)
        .expect("the TOON app is in the state it was changed in");
    let ilp_address = node::tip_prefix(&target.segment);
    let url = connector_url(home, target)?;
    let key = seal_key(home, target)?;
    let event = event::sign(
        &secret,
        event::now(),
        ROUTE,
        json!([
            ["connector_url", url],
            ["ilp_address", ilp_address],
            ["connector_seal_key", key],
        ]),
        "",
    )?;
    let published = publish(home, &relay, &event).map_err(|mut error| {
        error.message = format!(
            "The tip route is on {} and tips are accepted, but the tip route event is not \
             published everywhere: {} Run `toon tip enable` again to publish it.",
            target.name, error.message
        );
        error
    })?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "enabled": true,
            "toon_app": target.name,
            "ilp_address": ilp_address,
            "minimum": minimum,
            "connector_url": url,
            "restarted": restarted,
            "published": published,
        }),
        text: format!(
            "Tips are on. The tip route {ilp_address} of {} takes at least {minimum} base \
             units, and the tip route event {} is published to the agent node's own relay and \
             to {relay}. {}",
            target.name,
            event["id"].as_str().unwrap_or_default(),
            apps::restarted(restarted, &target.name)
        ),
    })
}

fn disable(home: &Path, yes: bool) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let Some(serving) = serving(&state) else {
        return Err(failed(
            ErrorCode::TipsNotEnabled,
            "This agent node does not take tips.".into(),
        ));
    };
    let name = serving.name.clone();
    let relay = profile_relay(&state)?;
    let secret = inbox::require_kept_secret(home)?;
    operator::surface_of(home, Some(&name)).map_err(|_| not_running())?;
    apps::confirm(yes, &name)?;

    let mut changed = state.clone();
    for toon_app in &mut changed.toon_apps {
        toon_app.tip = None;
    }
    let restarted = apps::apply(home, &state, &changed, &name, || {})?;
    let identity = event::public_key(&secret)?;
    let event = event::sign(
        &secret,
        event::now(),
        DELETION,
        json!([
            ["a", format!("{ROUTE}:{identity}:")],
            ["k", ROUTE.to_string()]
        ]),
        "",
    )?;
    let published = publish(home, &relay, &event).map_err(|mut error| {
        error.message = format!(
            "The tip route is gone from {name}, but its deletion is not published everywhere: \
             {} Publish it with `toon event publish --kind 5 --tags '[[\"a\",\"10850:<agent identity>:\"],[\"k\",\"10850\"]]'`.",
            error.message
        );
        error
    })?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "enabled": false,
            "toon_app": name,
            "restarted": restarted,
            "published": published,
        }),
        text: format!(
            "Tips are off. The tip route of {name} is removed, and its deletion {} is published \
             to the agent node's own relay and to {relay}. {}",
            event["id"].as_str().unwrap_or_default(),
            apps::restarted(restarted, &name)
        ),
    })
}

/// What `toon status` says of tips: a line of text and the JSON.
pub fn status(home: &Path, state: &State) -> (String, Value) {
    let queue = acknowledge::queue(home);
    let waiting = json!({ "waiting": queue.waiting, "last_error": queue.last_error });
    let Some(app) = serving(state) else {
        return (
            format!("Tips: off. Acknowledgements waiting: {}.", queue.waiting),
            json!({
                "enabled": false,
                "toon_app": null,
                "ilp_address": null,
                "minimum": null,
                "acknowledgements": waiting,
            }),
        );
    };
    let tip = app
        .tip
        .expect("serving() found a TOON app with a tip route");
    let ilp_address = node::tip_prefix(&app.segment);
    let mut text = format!(
        "Tips: on, at {ilp_address} of {}, from {} base units. Acknowledgements waiting: {}.",
        app.name, tip.minimum, queue.waiting
    );
    if let Some(error) = &queue.last_error {
        text.push_str(&format!(" The last could not be published: {error}"));
    }
    (
        text,
        json!({
            "enabled": true,
            "toon_app": app.name,
            "ilp_address": ilp_address,
            "minimum": tip.minimum,
            "acknowledgements": waiting,
        }),
    )
}
