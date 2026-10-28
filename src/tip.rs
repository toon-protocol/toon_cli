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
use crate::describe;
use crate::egress::Egress;
use crate::event;
use crate::inbox;
use crate::node::{self, Reach, State, Tip, ToonApp};
use crate::operator::{self, Answer};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::ports;
use crate::spending;
use crate::subscribe;

/// The kind of the event that names a tip route: replaceable, one per agent identity.
pub const ROUTE: u64 = 10_850;

/// The kind of the tip receipt the payer signs.
const RECEIPT: u64 = 9_850;

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
        TipCommand::Send {
            recipient,
            amount,
            r#for,
            message,
            anonymous,
            relay,
            yes,
        } => send(
            home,
            &TipSend {
                recipient,
                amount: *amount,
                what: r#for,
                message: message.as_deref(),
                anonymous: *anonymous,
                relays: relay,
                yes: *yes,
            },
        ),
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

/// What `toon tip send` was asked.
struct TipSend<'a> {
    recipient: &'a str,
    amount: u64,
    what: &'a [String],
    message: Option<&'a str>,
    anonymous: bool,
    relays: &'a [String],
    yes: bool,
}

fn usage(message: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::Usage,
        message,
    }
}

/// An error before a packet is sent.
fn unsent(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: true,
        ..failed(code, message)
    }
}

fn is_hex_key(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The 32 bytes an `npub` (NIP-19, bech32) encodes.
fn npub_key(npub: &str) -> Option<String> {
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    if npub.chars().any(char::is_uppercase) && npub.chars().any(char::is_lowercase) {
        return None;
    }
    let lower = npub.to_ascii_lowercase();
    let (human, data) = lower.rsplit_once('1')?;
    if human != "npub" {
        return None;
    }
    let values: Vec<u8> = data
        .bytes()
        .map(|byte| CHARSET.iter().position(|&c| c == byte).map(|at| at as u8))
        .collect::<Option<_>>()?;
    let polymod = |values: &[u8]| {
        const GENERATOR: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
        values.iter().fold(1u32, |checksum, &value| {
            let top = checksum >> 25;
            let mut checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(value);
            for (bit, generator) in GENERATOR.iter().enumerate() {
                if (top >> bit) & 1 == 1 {
                    checksum ^= generator;
                }
            }
            checksum
        })
    };
    let mut checked: Vec<u8> = human.bytes().map(|byte| byte >> 5).collect();
    checked.push(0);
    checked.extend(human.bytes().map(|byte| byte & 31));
    checked.extend(&values);
    if values.len() < 6 || polymod(&checked) != 1 {
        return None;
    }
    let payload = &values[..values.len() - 6];
    let (mut accumulator, mut bits, mut bytes) = (0u32, 0u32, Vec::new());
    for &value in payload {
        accumulator = (accumulator << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            bytes.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    // Padding is zero and shorter than a byte.
    (bytes.len() == 32 && bits < 5 && accumulator == 0).then(|| hex::encode(bytes))
}

/// The recipient's public key as 64 lowercase hex characters, from an `npub` or hex.
fn recipient_key(text: &str) -> Result<String, Error> {
    if is_hex_key(text) {
        return Ok(text.to_ascii_lowercase());
    }
    npub_key(text).ok_or_else(|| {
        usage(format!(
            "{text} is not an agent identity: give an `npub` or 64 hex characters."
        ))
    })
}

/// The tip receipt's `e` and `a` tags for what the tip is for.
fn subject_tags(what: &[String]) -> Result<Vec<Value>, Error> {
    what.iter()
        .map(|subject| {
            if is_hex_key(subject) {
                return Ok(json!(["e", subject.to_ascii_lowercase()]));
            }
            let mut parts = subject.splitn(3, ':');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(kind), Some(pubkey), Some(_))
                    if !kind.is_empty()
                        && kind.bytes().all(|byte| byte.is_ascii_digit())
                        && is_hex_key(pubkey) =>
                {
                    Ok(json!(["a", subject]))
                }
                _ => Err(usage(format!(
                    "--for {subject} is neither an event id (64 hex characters) nor an address \
                     `kind:pubkey:d`."
                ))),
            }
        })
        .collect()
}

/// A tip route as the event names it.
struct Route {
    connector_url: String,
    ilp_address: String,
    seal_key: [u8; 65],
}

fn route_of(event: &Value) -> Result<Route, String> {
    let named = |name: &str| {
        event["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_array())
            .find(|tag| tag.first().and_then(Value::as_str) == Some(name))
            .and_then(|tag| tag.get(1))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("no `{name}`"))
    };
    let key = named("connector_seal_key")?;
    let seal_key = hex::decode(key.trim_start_matches("0x"))
        .ok()
        .and_then(|bytes| <[u8; 65]>::try_from(bytes).ok())
        .ok_or_else(|| format!("a `connector_seal_key` that is not 65 bytes of hex: {key}"))?;
    Ok(Route {
        connector_url: named("connector_url")?.to_owned(),
        ilp_address: named("ilp_address")?.to_owned(),
        seal_key,
    })
}

/// The relays a tip route is looked for on: the agent node's own, the one its profile follows
/// and the ones the operator names.
fn relays_to_read(home: &Path, state: &State, named: &[String]) -> (Vec<String>, Vec<String>) {
    let mut relays = Vec::new();
    let mut unread = Vec::new();
    let own = subscribe::own_relay_app(state)
        .and_then(|app| subscribe::own_relay_address(home, &app.name, subscribe::RelayPort::Read));
    match own {
        Ok(address) => relays.push(format!("ws://{address}")),
        Err(error) => unread.push(format!("the agent node's own relay: {}", error.message)),
    }
    relays.extend(state.relay_url.clone());
    relays.extend(named.iter().cloned());
    let mut seen = Vec::new();
    relays.retain(|relay| {
        let new = !seen.contains(relay);
        seen.push(relay.clone());
        new
    });
    (relays, unread)
}

/// The newest tip route (kind 10850) `recipient` signed that `relays` hold; of two at the
/// same second, the lower id, as NIP-01 keeps.
fn find_route(
    home: &Path,
    state: &State,
    recipient: &str,
    named: &[String],
) -> Result<(Value, Route), Error> {
    let (relays, mut unread) = relays_to_read(home, state, named);
    let egress = Egress::of(home)?;
    let filter = json!({ "kinds": [ROUTE], "authors": [recipient] });
    let mut found: Vec<Value> = Vec::new();
    for relay in &relays {
        match event::fetch(&egress, relay, &filter) {
            Ok(events) => found.extend(events.into_iter().filter(|event| {
                event["kind"] == ROUTE
                    && event["pubkey"]
                        .as_str()
                        .map(str::to_ascii_lowercase)
                        .as_deref()
                        == Some(recipient)
                    && event::verify(event).is_ok()
            })),
            Err(error) => unread.push(format!("{relay}: {}", error.message)),
        }
    }
    let at = |event: &Value| event["created_at"].as_u64().unwrap_or(0);
    let id = |event: &Value| event["id"].as_str().unwrap_or("").to_owned();
    let newest = found.into_iter().max_by(|one, other| {
        at(one)
            .cmp(&at(other))
            .then_with(|| id(other).cmp(&id(one)))
    });
    let Some(newest) = newest else {
        let reads = if unread.is_empty() {
            String::new()
        } else {
            format!(" These could not be read: {}", unread.join("; "))
        };
        return Err(unsent(
            ErrorCode::TipRouteNotFound,
            format!(
                "No tip route (kind 10850) of {recipient} is on {}. The recipient may not take \
                 tips, or publishes the tip route elsewhere: name a relay with `--relay`.{reads}",
                relays.join(", ")
            ),
        ));
    };
    let route = route_of(&newest).map_err(|why| {
        unsent(
            ErrorCode::TipRouteNotFound,
            format!(
                "The newest tip route of {recipient}, {}, is unusable: {why}.",
                id(&newest)
            ),
        )
    })?;
    Ok((newest, route))
}

/// A price as the connector's self-description states it, a number or a decimal string.
fn price_of(route: &Value) -> Option<u128> {
    match &route["price"] {
        Value::String(text) => text.parse().ok(),
        other => other.as_u64().map(u128::from),
    }
}

/// The cost of the path to the tip route: what the connectors that forward it keep, and
/// whether that is the whole of it. A packet of no amount learns it from the reject, as
/// `toon probe` does, and moves nothing. A path with several connectors that charge states only
/// the amount to carry past the first, which is then the least that is known of the cost.
fn path_cost(home: &Path, route: &Route) -> Result<(u128, bool), Error> {
    let answer = operator::dispatch_with_headers(
        home,
        &route.ilp_address,
        0,
        &route.seal_key,
        ("POST", "/"),
        json_headers(),
        Vec::new(),
    )?;
    let cannot = |why: String| {
        unsent(
            ErrorCode::SendFailed,
            format!(
                "The cost of the path to {} could not be learned: {why}",
                route.ilp_address
            ),
        )
    };
    match answer {
        Answer::Fulfilled { .. } => Ok((0, true)),
        Answer::Rejected {
            code,
            message,
            cost,
        } => match connector_cli::reject_cost_reading(&code) {
            connector_cli::CostReading::Complete => Ok((cost, true)),
            connector_cli::CostReading::Partial => Ok((cost, false)),
            connector_cli::CostReading::NoAnswer => Err(cannot(format!(
                "the probe was rejected with {code}: {message}"
            ))),
        },
        Answer::WrongFulfilment => Err(cannot("the fulfilment did not match.".into())),
        Answer::Unanswered => Err(cannot(operator::unanswered_message())),
    }
}

fn json_headers() -> Vec<(String, String)> {
    vec![("content-type".to_owned(), "application/json".to_owned())]
}

/// `toon tip send`.
fn send(home: &Path, tip: &TipSend) -> Result<Report, Error> {
    let recipient = recipient_key(tip.recipient)?;
    let subjects = subject_tags(tip.what)?;
    let state = State::load(home)?.ok_or_else(|| node::no_agent_node(home))?;
    let unit = state
        .toon_apps
        .first()
        .and_then(|app| app.evm.as_ref())
        .and_then(node::Evm::unit)
        .ok_or_else(|| {
            unsent(
                ErrorCode::SendFailed,
                "The token this agent node pays in has no asset code, so a tip receipt cannot \
                 state what it tips. Nothing was sent."
                    .into(),
            )
        })?;

    // Find the tip route, and check it against the connector it names.
    let (_, route) = find_route(home, &state, &recipient, tip.relays)?;
    let egress = Egress::of(home)?;
    let description = describe::fetch(&egress, &route.connector_url)?;
    let listed = description["routes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|listed| listed["prefix"] == route.ilp_address.as_str() && listed["payment"] == true)
        .ok_or_else(|| {
            unsent(
                ErrorCode::TipRouteNotListed,
                format!(
                    "The connector at {} does not list {} as a payment route, so a tip to it \
                     would not be taken. Nothing was sent.",
                    route.connector_url, route.ilp_address
                ),
            )
        })?;
    let minimum = price_of(listed).ok_or_else(|| {
        unsent(
            ErrorCode::TipRouteNotListed,
            format!(
                "The connector at {} states no price for {}. Nothing was sent.",
                route.connector_url, route.ilp_address
            ),
        )
    })?;
    let served = operator::identity(&egress, &route.connector_url)?;
    if served != route.seal_key {
        return Err(unsent(
            ErrorCode::TipSealKeyMismatch,
            format!(
                "The connector at {} gives a sealing key other than the `connector_seal_key` the \
                 tip route names. Nothing was sent.",
                route.connector_url
            ),
        ));
    }

    // Price it.
    if !operator::forwards(home, &route.ilp_address)? {
        return Err(operator::peering_needed(
            home,
            &route.ilp_address,
            &route.connector_url,
            format!(
                "No peering of this agent node reaches {}, where the tip is taken. A peering is \
                 needed: run `{}` and then `toon route add {} --peer <id>`.",
                route.ilp_address,
                operator::peer_add_command(&route.connector_url, "<amount>", ""),
                route.ilp_address
            ),
        ));
    }
    let (cost, whole) = path_cost(home, &route)?;
    let sent = u128::from(tip.amount);
    let least = cost + minimum;
    if sent < least {
        return Err(unsent(
            ErrorCode::TipBelowMinimum,
            format!(
                "--amount {} is too little: the path costs {cost} and the tip route takes at \
                 least {minimum} of what arrives, so the least `--amount` that would do is \
                 {least}. Nothing was sent.",
                tip.amount
            ),
        ));
    }

    // Write the receipt. It is never published to a relay: it travels in the packet.
    let secret = event::agent_secret(home)?;
    event::keep_agent_secret(home, &secret)?;
    let mut tags = vec![json!(["p", recipient])];
    tags.extend(subjects);
    tags.push(json!([
        "amount",
        tip.amount.to_string(),
        unit.code,
        unit.decimals.to_string()
    ]));
    if tip.anonymous {
        tags.push(json!(["anon"]));
    }
    let receipt = event::sign(
        &secret,
        event::now(),
        RECEIPT,
        Value::Array(tags),
        tip.message.unwrap_or_default(),
    )?;
    let receipt_id = receipt["id"].as_str().unwrap_or_default().to_owned();

    // Send it, sealed to the pinned key, which is not fetched again.
    spending::spend_packets(home, sent, tip.yes, |packets| {
        let answer = operator::dispatch_with_headers(
            home,
            &route.ilp_address,
            tip.amount,
            &route.seal_key,
            ("POST", "/"),
            json_headers(),
            receipt.to_string().into_bytes(),
        )
        .map_err(|error| operator::repriced(error, packets.moved(sent)))?;
        let report = match answer {
            Answer::Unanswered => {
                return Err(operator::unanswered_cost(packets.moved(sent), None));
            }
            Answer::Fulfilled { status, body } => Report {
                exit: Exit::Success,
                json: json!({
                    "outcome": "fulfilled",
                    "sent": tip.amount,
                    "path_cost": cost,
                    "path_cost_complete": whole,
                    "arrives_about": sent - cost,
                    "receipt_id": receipt_id,
                    "recipient": recipient,
                    "ilp_address": route.ilp_address,
                    "paid": tip.amount,
                    "response": { "status": status, "body": body },
                }),
                text: format!(
                    "Sent {}, path cost {}{cost}, about {} arrives. The tip counts once {recipient} \
                     acknowledges receipt {receipt_id}.",
                    tip.amount,
                    if whole { "" } else { "at least " },
                    sent - cost
                ),
            },
            Answer::Rejected {
                code,
                message,
                cost: stated,
            } => {
                let paid = packets.moved(sent);
                let mut json = json!({
                    "outcome": "rejected",
                    "sent": tip.amount,
                    "receipt_id": receipt_id,
                    "recipient": recipient,
                    "ilp_address": route.ilp_address,
                    "reject": { "code": code, "message": message },
                    "paid": paid,
                });
                operator::add_cost(&mut json, &code, stated);
                let mut text = format!(
                    "Rejected with {code}: {} base units to {}. {message}{}",
                    tip.amount,
                    route.ilp_address,
                    operator::cost_sentence(&code, stated, "--amount")
                );
                if paid > 0 {
                    text.push_str(&format!(" It cost {paid} base units."));
                }
                Report {
                    exit: Exit::Failure,
                    json,
                    text,
                }
            }
            Answer::WrongFulfilment => Report {
                exit: Exit::Failure,
                json: json!({
                    "outcome": "wrong_fulfilment",
                    "sent": tip.amount,
                    "receipt_id": receipt_id,
                    "recipient": recipient,
                    "ilp_address": route.ilp_address,
                    "paid": packets.moved(sent),
                }),
                text: "Fulfilled, but not by this connector: the fulfilment did not match."
                    .to_owned(),
            },
        };
        let paid = report.json["paid"].as_u64().map_or(sent, u128::from);
        Ok((report, paid))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";
    const NPUB: &str = "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg";

    #[test]
    fn an_npub_and_its_hex_name_the_same_recipient() {
        assert_eq!(recipient_key(NPUB).unwrap(), HEX);
        assert_eq!(recipient_key(&HEX.to_uppercase()).unwrap(), HEX);
    }

    #[test]
    fn a_damaged_npub_or_another_word_is_not_a_recipient() {
        let damaged = NPUB.replace("qzvjptg", "qzvjpth");
        for text in [damaged.as_str(), "npub1", "nsec10elfcs4", "abc", ""] {
            assert_eq!(recipient_key(text).unwrap_err().code, ErrorCode::Usage);
        }
    }

    #[test]
    fn a_subject_is_an_event_id_or_an_address_and_nothing_else() {
        let address = format!("30023:{HEX}:note");
        let tags = subject_tags(&[HEX.to_owned(), address.clone(), format!("1:{HEX}:")]).unwrap();
        assert_eq!(tags[0], json!(["e", HEX]));
        assert_eq!(tags[1], json!(["a", address]));
        assert_eq!(tags[2][0], "a");
        for bad in ["note1abc", "x:y:z", &format!("30023:{HEX}"), "ab"] {
            assert!(subject_tags(&[bad.to_owned()]).is_err(), "{bad}");
        }
    }
}
