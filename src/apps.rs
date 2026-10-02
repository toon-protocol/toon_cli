//! `toon add`, `toon remove` and `toon route price`: the commands that change the routes
//! a connector terminates.
//!
//! The connector reads its routes only at start (ADR 0002), so each of these records the
//! change in the state and then asks the supervisor to start the connector again. That
//! drops the packets it holds in flight, so each refuses without `--yes`. If the restart
//! fails, the state is put back and the connector is started again as it was. If the
//! agent node is not running, the change is checked and recorded, and `toon up` starts
//! the connector with it.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::control;
use crate::node::{self, App, Source, State, ToonApp};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::runner;

/// How long a restart waits: an image is pulled and started, then the connector binds to
/// its chain.
const RESTART_PATIENCE: Duration = Duration::from_secs(300);

/// Where an app added by `toon add` comes from.
pub enum Origin<'a> {
    Image(&'a str),
    Url(&'a str),
}

impl Origin<'_> {
    /// Refuses the relay's image: an agent node runs one relay, the one `toon init` created.
    pub fn refuse_relay_image(&self) -> Result<(), Error> {
        match self {
            Origin::Image(image) if repository(image) == repository(env!("TOON_RELAY_IMAGE")) => {
                Err(failed(
                    ErrorCode::OneRelay,
                    "An agent node runs one relay, and it is the one `toon init` created: \
                     `toon create` and `toon add` cannot start another."
                        .into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

/// An image reference without its digest and its tag.
fn repository(image: &str) -> &str {
    let image = image.split_once('@').map_or(image, |(name, _)| name);
    match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => name,
        _ => image,
    }
}

/// What `toon add` was asked for.
pub struct Add<'a> {
    pub name: &'a str,
    pub to: &'a str,
    pub origin: Origin<'a>,
    /// The address prefix, `g.toon.<segment>.<app>` if omitted.
    pub address: Option<&'a str>,
    pub price: u64,
    pub yes: bool,
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
        unanswered: None,
    }
}

fn loaded(home: &Path) -> Result<State, Error> {
    State::load(home)?.ok_or_else(|| node::no_agent_node(home))
}

/// A name or an address prefix that is one URL path segment and one file name, as the
/// connector's operator surface and the app's directory both need.
fn plain(code: ErrorCode, what: &str, text: &str) -> Result<(), Error> {
    let ok = !text.is_empty()
        && !text.starts_with('.')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if ok {
        return Ok(());
    }
    Err(failed(
        code,
        format!(
            "{what} {text:?} must be letters, digits, `.`, `-` and `_`, and not start with `.`."
        ),
    ))
}

fn warning(toon_app: &str) -> String {
    format!(
        "This restarts the connector of the TOON app {toon_app}, and drops the packets it \
         holds in flight."
    )
}

fn confirm(yes: bool, toon_app: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    Err(failed(
        ErrorCode::ConfirmationRequired,
        format!(
            "{} Run it again with `--yes` to go ahead.",
            warning(toon_app)
        ),
    ))
}

/// Record `changed` as the state, and start the connector again if it runs. If it does not
/// start again with the change, the state is put back, and the connector, if the failed
/// restart stopped it, started again without it. If nothing runs, the change is checked
/// against the connector's own validation before it is recorded. Returns whether a
/// connector was restarted.
pub fn apply(home: &Path, before: &State, changed: &State, toon_app: &str) -> Result<bool, Error> {
    if !control::running(home) {
        check(home, changed, toon_app)?;
        changed.save(home)?;
        return Ok(false);
    }
    changed.save(home)?;
    let reply = control::ask_about(home, "reload", Some(toon_app), RESTART_PATIENCE);
    let mut error = match &reply {
        Some(reply) if reply["reloaded"] == true => return Ok(true),
        Some(reply) if reply["error"].is_object() => {
            let code = match reply["error"]["code"].as_str() {
                Some("app_failed") => ErrorCode::AppFailed,
                _ => ErrorCode::ConnectorFailed,
            };
            failed(
                code,
                reply["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        }
        _ => failed(
            ErrorCode::ConnectorFailed,
            format!("The supervisor did not restart the connector of {toon_app}."),
        ),
    };
    before.save(home)?;
    // A restart that failed before it stopped the connector left it as it was.
    let untouched = reply.is_some_and(|reply| reply["stopped"] == false);
    if !untouched {
        let back = control::ask_about(home, "reload", Some(toon_app), RESTART_PATIENCE);
        if !back.is_some_and(|back| back["reloaded"] == true) {
            error.message.push_str(&format!(
                " The connector of {toon_app} did not start again as it was either."
            ));
        }
    }
    Err(error)
}

/// Render the config of `toon_app` in `changed`, with each app that runs reached where its
/// container serves, and check it with the connector's own validation.
pub fn check(home: &Path, changed: &State, toon_app: &str) -> Result<(), Error> {
    let Some(app) = changed.toon_apps.iter().find(|app| app.name == toon_app) else {
        return Ok(());
    };
    let placeholder = SocketAddr::from((Ipv4Addr::LOCALHOST, runner::WRITE_PORT));
    let addresses: Vec<(String, SocketAddr)> = app
        .apps
        .iter()
        .map(|behind| (behind.name.clone(), placeholder))
        .collect();
    // A hidden service renders only with its overlay, which a check has none of: a stand-in
    // is enough to check what the config says.
    let overlay = matches!(app.reach, node::Reach::Hidden).then(|| node::Overlay {
        proxy: placeholder,
        endpoint: "check.anyone".into(),
    });
    node::render(home, app, &addresses, overlay.as_ref()).map(|_| ())
}

fn restarted(now: bool, toon_app: &str) -> String {
    if now {
        format!("The connector of {toon_app} restarted.")
    } else {
        format!(
            "The agent node is not running: the connector of {toon_app} starts with these routes."
        )
    }
}

/// Whether `name` is usable as the name of a TOON app or an app, and no TOON app or app of
/// this agent node has it.
pub fn free(state: &State, name: &str) -> Result<(), Error> {
    plain(ErrorCode::NameTaken, "The name", name)?;
    if state
        .toon_apps
        .iter()
        .any(|toon_app| toon_app.name == name || toon_app.apps.iter().any(|app| app.name == name))
    {
        return Err(failed(
            ErrorCode::NameTaken,
            format!("This agent node already has a TOON app or an app called {name}."),
        ));
    }
    Ok(())
}

/// `toon add`: put a new app behind the connector of the TOON app `to`.
pub fn add(home: &Path, add: &Add) -> Result<Report, Error> {
    add.origin.refuse_relay_image()?;
    let state = loaded(home)?;
    let Some(index) = state.toon_apps.iter().position(|app| app.name == add.to) else {
        return Err(unknown(&state, add.to));
    };
    free(&state, add.name)?;
    let prefix = add.address.map_or_else(
        || node::app_address(&state.toon_apps[index].segment, add.name),
        str::to_owned,
    );
    plain(ErrorCode::RouteFailed, "The address", &prefix)?;
    let taken = |prefix: &str| {
        let ours = &state.toon_apps[index];
        ours.apps.iter().any(|app| {
            app.prefix == prefix
                || (app.source == Source::Relay
                    && prefix == node::relay_ephemeral_prefix(&ours.segment))
        })
    };
    if taken(&prefix) {
        return Err(failed(
            ErrorCode::RouteFailed,
            format!("The connector of {} already terminates {prefix}.", add.to),
        ));
    }
    let source = match add.origin {
        Origin::Image(image) => Source::Image(image.to_owned()),
        Origin::Url(url) => Source::Url(url.to_owned()),
    };
    confirm(add.yes, add.to)?;
    let mut changed = state.clone();
    changed.toon_apps[index].apps.push(App {
        name: add.name.to_owned(),
        source,
        prefix: prefix.clone(),
        price: add.price,
    });
    let restarted_now = apply(home, &state, &changed, add.to)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "added": add.name,
            "toon_app": add.to,
            "address": prefix,
            "price": add.price,
            "restarted": restarted_now,
        }),
        text: format!(
            "Added {} behind {}: packets to {prefix} are delivered to it, at price {}. {}",
            add.name,
            add.to,
            add.price,
            restarted(restarted_now, add.to)
        ),
    })
}

/// The TOON app called `name`, or the first one if `None`.
pub fn toon_app<'a>(state: &'a State, name: Option<&str>) -> Result<&'a ToonApp, Error> {
    match name {
        None => Ok(&state.toon_apps[0]),
        Some(name) => state
            .toon_apps
            .iter()
            .find(|app| app.name == name)
            .ok_or_else(|| unknown(state, name)),
    }
}

pub fn unknown(state: &State, name: &str) -> Error {
    let known: Vec<&str> = state
        .toon_apps
        .iter()
        .map(|app| app.name.as_str())
        .collect();
    failed(
        ErrorCode::UnknownName,
        format!(
            "No TOON app is called {name}. This agent node has {}.",
            known.join(", ")
        ),
    )
}

/// `toon remove`: take the app `name`, and its route, away from the connector it is behind.
pub fn remove(home: &Path, name: &str, yes: bool) -> Result<Report, Error> {
    let state = loaded(home)?;
    let Some((index, position)) = state.toon_apps.iter().enumerate().find_map(|(index, app)| {
        app.apps
            .iter()
            .position(|behind| behind.name == name)
            .map(|position| (index, position))
    }) else {
        let known: Vec<&str> = state
            .toon_apps
            .iter()
            .flat_map(|app| app.apps.iter().map(|behind| behind.name.as_str()))
            .collect();
        return Err(failed(
            ErrorCode::UnknownName,
            format!(
                "No app is called {name}. This agent node has {}.",
                known.join(", ")
            ),
        ));
    };
    let toon_app = state.toon_apps[index].name.clone();
    confirm(yes, &toon_app)?;
    let mut changed = state.clone();
    let gone = changed.toon_apps[index].apps.remove(position);
    let restarted_now = apply(home, &state, &changed, &toon_app)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "removed": name,
            "toon_app": toon_app,
            "address": gone.prefix,
            "restarted": restarted_now,
        }),
        text: format!(
            "Removed {name} from {toon_app}, and its route {}. {}",
            gone.prefix,
            restarted(restarted_now, &toon_app)
        ),
    })
}

/// `toon route price`: set what a client pays for a packet on the route to an app.
pub fn route_price(home: &Path, prefix: &str, price: u64, yes: bool) -> Result<Report, Error> {
    let state = loaded(home)?;
    let found: Option<(usize, usize)> =
        state.toon_apps.iter().enumerate().find_map(|(index, app)| {
            app.apps
                .iter()
                .position(|behind| behind.prefix == prefix)
                .map(|position| (index, position))
        });
    let Some((index, position)) = found else {
        return Err(failed(
            ErrorCode::RouteFailed,
            format!(
                "No app is reached at {prefix}. `toon route price` sets the price of the route to \
                 an app; `toon route add` sets the price of a route to a peering."
            ),
        ));
    };
    let toon_app = state.toon_apps[index].name.clone();
    confirm(yes, &toon_app)?;
    let mut changed = state.clone();
    changed.toon_apps[index].apps[position].price = price;
    let restarted_now = apply(home, &state, &changed, &toon_app)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "prefix": prefix,
            "price": price,
            "toon_app": toon_app,
            "restarted": restarted_now,
        }),
        text: format!(
            "The route {prefix} costs {price}. {}",
            restarted(restarted_now, &toon_app)
        ),
    })
}
