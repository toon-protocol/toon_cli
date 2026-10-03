//! The connector's operator surface, used the way a remote operator would use it.
//!
//! Reads carry the connector's bearer token. A write is signed with the wallet's
//! operator write key, whose public half the connector's config lists. Both go over
//! loopback to the address the supervisor says its connector listens on.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::cli::{Chain, ChannelCommand, JoinArgs, PeerCommand, RouteCommand};
use crate::control;
use crate::egress::Egress;
use crate::funding::GasRefusal;
use crate::node::{self, ConnectorFiles, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::profile::Profile;
use crate::spending;

/// How long a read of the operator surface waits for the connector.
const PATIENCE: Duration = Duration::from_secs(30);

/// A running connector's operator surface.
pub struct Surface {
    /// `http://host:port`, with no trailing slash.
    pub url: String,
    bearer_token: PathBuf,
    write_key: PathBuf,
    /// Whether the connector may peer toward a plain `http://` address.
    plaintext_peers: bool,
    /// What the connector's key must hold for gas, to name when a write is refused for it.
    gas: Option<GasRefusal>,
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
        unanswered: None,
    }
}

/// The TOON app every command that talks to a connector is about, when the operator named
/// one with `--app`: the first TOON app's otherwise.
static TARGET: OnceLock<String> = OnceLock::new();

/// Name the TOON app this process's commands are about.
pub fn target(app: Option<&str>) {
    if let Some(app) = app {
        let _ = TARGET.set(app.to_owned());
    }
}

/// The operator surface of the TOON app named by `--app`, or the first one, whose
/// connector must be running.
fn surface(home: &Path) -> Result<Surface, Error> {
    surface_of(home, TARGET.get().map(String::as_str))
}

/// The operator surface of the connector of the TOON app `name`, or of the first TOON app,
/// which must be running.
pub fn surface_of(home: &Path, name: Option<&str>) -> Result<Surface, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let app = crate::apps::toon_app(&state, name)?;
    let address = control::ask(home, "status")
        .and_then(|reply| {
            reply["toon_apps"]
                .as_array()?
                .iter()
                .find(|reported| reported["name"] == app.name.as_str())
                .and_then(|reported| reported["connector"]["address"].as_str().map(str::to_owned))
        })
        .ok_or_else(|| {
            failed(
                ErrorCode::NotRunning,
                "The agent node is not running. Run `toon up`.".into(),
            )
        })?;
    Ok(Surface {
        url: format!("http://{address}"),
        bearer_token: ConnectorFiles::of(home, app.connector).bearer_token,
        write_key: node::operator_key(home),
        plaintext_peers: app.plaintext_peers,
        gas: GasRefusal::of(home, state.network, app),
    })
}

/// A read of the connector's operator surface: `path`, with the bearer token.
fn read(surface: &Surface, path: &str) -> Result<Vec<Value>, Error> {
    let token = std::fs::read_to_string(&surface.bearer_token).map_err(|source| {
        failed(
            ErrorCode::Io,
            format!("{}: {source}.", surface.bearer_token.display()),
        )
    })?;
    let url = format!("{}{path}", surface.url);
    reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .and_then(|client| client.get(&url).bearer_auth(token.trim()).send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| failed(ErrorCode::ConnectorFailed, format!("GET {url}: {error}.")))
}

/// How long a signed write stays valid, in seconds.
const SIGNATURE_TTL: u64 = 60;

/// How long a write waits for the connector: a peering opens and funds a channel, which
/// waits for the chain.
const WRITE_PATIENCE: Duration = Duration::from_secs(300);

/// What the connector says when it refuses a write for a signature it has accepted before.
/// The repeat of a write is keyed on it; pinned by a test against the connector the build
/// embeds.
pub(crate) const REPLAYED: &str = "signature has already been used";

/// How long a write that was refused as a replay waits before it is signed again: until the
/// clock's second has turned, which is at most a second, and a little over for the clock's
/// own slack.
const REPLAY_SLACK: Duration = Duration::from_millis(20);

/// Wait until the clock's second has turned, so that a write signed again is signed with
/// another `created` and so another signature.
fn wait_for_next_second() {
    let into_second = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(Duration::ZERO, |elapsed| {
            Duration::from_nanos(u64::from(elapsed.subsec_nanos()))
        });
    std::thread::sleep(Duration::from_secs(1) - into_second + REPLAY_SLACK);
}

/// What a write refused as a replay twice says in place of the connector's own text.
fn replayed_twice(first: &str) -> String {
    format!(
        "{}. The connector had already accepted this same write in this second, and \
         refused it again when it was signed in the next. Nothing was done. Run the command \
         again.",
        first.trim().trim_end_matches('.')
    )
}

/// Whether `status` and `text` are the connector refusing a write as a replay.
fn is_replay(status: u16, text: &str) -> bool {
    status == 401 && text.contains(REPLAYED)
}

/// A write to the connector's operator surface, signed with the wallet's operator write
/// key. `body` is JSON, or empty for a delete. Returns the status and the body.
fn write(
    surface: &Surface,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, String), Error> {
    write_text(
        surface,
        method,
        path,
        body.map(Value::to_string).unwrap_or_default(),
        WRITE_PATIENCE,
    )
}

/// [`write`] with the body as JSON text, which holds an amount above `u64::MAX` that a
/// `Value` does not, waiting `patience` for the connector.
///
/// A signature covers the method, the path, the body, the second it was made in and the
/// key, so the same write twice in one second has the same signature, and the connector
/// refuses the second as a replay. That refusal is waited out once: after the second has
/// turned the write is signed again and sent again. A connector's 401 is the first thing
/// it checks, so a write refused that way did nothing.
fn write_text(
    surface: &Surface,
    method: reqwest::Method,
    path: &str,
    body: String,
    patience: Duration,
) -> Result<(u16, String), Error> {
    let keypair = write_keypair(&surface.write_key)?;
    let url = format!("{}{path}", surface.url);
    let client = reqwest::blocking::Client::builder()
        .timeout(patience)
        .build()
        .map_err(|error| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{method} {url}: {error}."),
            )
        })?;
    let sign_and_send = || -> Result<(u16, String), Error> {
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let (signature_input, signature, content_digest) =
            connector_operator::signing::sign_request(
                &keypair,
                method.as_str(),
                path,
                body.as_bytes(),
                created,
                Some(created + SIGNATURE_TTL),
            );
        let mut request = client
            .request(method.clone(), &url)
            .header("content-digest", content_digest)
            .header("signature-input", signature_input)
            .header("signature", signature);
        if !body.is_empty() {
            request = request
                .header("content-type", "application/json")
                .body(body.clone());
        }
        let response = request.send().map_err(|error| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{method} {url}: {error}."),
            )
        })?;
        let status = response.status().as_u16();
        let text = response.text().map_err(|error| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{method} {url}: {error}."),
            )
        })?;
        Ok((status, text))
    };
    let (status, text) = sign_and_send()?;
    if !is_replay(status, &text) {
        return Ok((status, text));
    }
    wait_for_next_second();
    let (status, again) = sign_and_send()?;
    if is_replay(status, &again) {
        return Ok((status, replayed_twice(&again)));
    }
    Ok((status, again))
}

/// `toon route list`: the connector's routing table, as it answers it: the routes it
/// terminates, then the routes it forwards.
pub fn route_list(home: &Path) -> Result<Report, Error> {
    let surface = surface(home)?;
    let mut routes = read(&surface, "/routes")?;
    let forwarding = read(&surface, "/routes/peers")?;
    // The operator surface lists a route without its `request`, which the state keeps.
    if let Some(state) = State::load(home)? {
        let toon_app = crate::apps::toon_app(&state, TARGET.get().map(String::as_str))?;
        for route in routes
            .iter_mut()
            .filter(|route| route.get("request").is_none())
        {
            let request = toon_app
                .apps
                .iter()
                .find(|app| route["prefix"].as_str() == Some(app.prefix.as_str()))
                .and_then(|app| app.request.clone());
            if let Some(request) = request {
                route["request"] = request;
            }
        }
    }
    let mut lines: Vec<String> = routes
        .iter()
        .map(|route| {
            let request = route
                .get("request")
                .map_or(String::new(), |request| format!(", request {request}"));
            format!(
                "{} -> {} (price {}{request})",
                route["prefix"].as_str().unwrap_or_default(),
                route["handler_url"].as_str().unwrap_or_default(),
                route["price"]
            )
        })
        .chain(forwarding.iter().map(|route| {
            format!(
                "{} -> peer {} (price {})",
                route["prefix"].as_str().unwrap_or_default(),
                route["peer_id"].as_str().unwrap_or_default(),
                route["price"]
            )
        }))
        .collect();
    if lines.is_empty() {
        lines.push("The connector has no routes.".into());
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "routes": routes, "forwarding_routes": forwarding }),
        text: lines.join("\n"),
    })
}

/// Whether `prefix` covers `address`: the same address, or one beneath it.
fn covers(prefix: &str, address: &str) -> bool {
    address == prefix
        || address
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a packet to `address` would leave through a peering: the longest route that
/// covers it forwards to a peer, and does not end at an app of this connector.
pub fn forwards(home: &Path, address: &str) -> Result<bool, Error> {
    let surface = surface(home)?;
    let prefix_of = |route: &Value| route["prefix"].as_str().unwrap_or_default().to_owned();
    let longest = |routes: &[Value]| {
        routes
            .iter()
            .map(prefix_of)
            .filter(|prefix| covers(prefix, address))
            .map(|prefix| prefix.len())
            .max()
    };
    let terminating = longest(&read(&surface, "/routes")?);
    let forwarding = longest(&read(&surface, "/routes/peers")?);
    Ok(match (forwarding, terminating) {
        (Some(forwarding), Some(terminating)) => forwarding >= terminating,
        (Some(_), None) => true,
        (None, _) => false,
    })
}

/// A channel's amount as the connector reports it, which is absent while it is opening or
/// when the chain could not be read.
fn amount(value: &Value) -> String {
    match value {
        Value::Null => "unknown".into(),
        other => other.to_string(),
    }
}

pub fn describe_channel(channel: &Value) -> String {
    format!(
        "{} {} {} {} (counterparty {}, collateral {}, landed {}, watermark {})",
        channel["id"].as_str().unwrap_or_default(),
        channel["chain"].as_str().unwrap_or_default(),
        channel["direction"].as_str().unwrap_or_default(),
        channel["status"].as_str().unwrap_or_default(),
        channel["counterparty"].as_str().unwrap_or_default(),
        amount(&channel["collateral"]),
        amount(&channel["landed"]),
        amount(&channel["watermark"]),
    )
}

/// Every channel the connector whose operator surface is `surface` holds.
pub fn channels_on(surface: &Surface) -> Result<Vec<Value>, Error> {
    read(surface, "/channels")
}

/// What the watermarks of the outbound channels of the connector add up to. An outbound
/// channel without a watermark counts as 0; one whose watermark is not a whole number is
/// `None`.
pub fn outbound_watermark(home: &Path) -> Result<Option<u128>, Error> {
    Ok(channels_on(&surface(home)?)?
        .iter()
        .filter(|channel| channel["direction"] == "outbound")
        .map(|channel| match &channel["watermark"] {
            Value::Null => Some(0),
            Value::Number(number) => number.as_u64().map(u128::from),
            Value::String(text) => text.parse().ok(),
            _ => None,
        })
        .sum())
}

/// `toon channel list`: every channel the connector holds, inbound and outbound.
pub fn channel_list(home: &Path) -> Result<Report, Error> {
    let channels = channels_on(&surface(home)?)?;
    let mut lines: Vec<String> = channels.iter().map(describe_channel).collect();
    if lines.is_empty() {
        lines.push("The connector has no channels.".into());
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "channels": channels }),
        text: lines.join("\n"),
    })
}

/// A label for a peering that names its address: the connector's URL without its scheme,
/// reduced to the characters a label can hold.
fn label(address: &str) -> String {
    let authority = address
        .split_once("://")
        .map_or(address, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default();
    authority
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// What the connector said about a refused write, for a person.
fn refusal(status: u16, text: &str) -> String {
    format!("The connector answered {status}: {}", text.trim())
}

/// A write the connector refused, as an error. A 401 comes from the connector checking
/// the write's signature before it does anything, so such a write moved nothing.
fn refused(code: ErrorCode, status: u16, text: &str) -> Error {
    Error {
        nothing_sent: status == 401,
        ..failed(code, refusal(status, text))
    }
}

impl Surface {
    /// The `unfunded` refusal if the connector refused a write for lack of gas.
    fn refused_for_gas(&self, status: u16, text: &str) -> Option<Error> {
        self.gas
            .as_ref()
            .and_then(|gas| gas.of_answer(status, text))
    }

    /// A write the connector refused, as an error: `unfunded` when the answer is a refusal
    /// for lack of gas, else `code`.
    fn refused_write(&self, code: ErrorCode, status: u16, text: &str) -> Error {
        self.refused_for_gas(status, text)
            .unwrap_or_else(|| refused(code, status, text))
    }
}

/// The `toon peer add` command a message tells the reader to run: it takes the deposit
/// `peer add` requires and the `--yes` it needs to deposit it. `deposit` and `extra` are
/// printed as given, so a placeholder in angle brackets may stand for either.
pub fn peer_add_command(address: &str, deposit: &str, extra: &str) -> String {
    format!("toon peer add {address} --deposit {deposit} --yes{extra}")
}

/// What `toon peer add` was asked for.
pub struct PeerAdd<'a> {
    pub address: &'a str,
    pub deposit: u128,
    pub id: Option<&'a str>,
    pub fee: u64,
    pub max_packet_amount: u64,
    /// The settlement chain to peer on, when the operator names one.
    pub chain: Option<Chain>,
}

/// What the connector's answer to a peering is when this connector and the other settle on
/// more than one chain and the request named none: the end of its refusal, which it makes
/// before any channel is opened. The chains on offer sit between [`AMBIGUOUS_CHAIN_LIST`]
/// and this. Pinned by a test against the connector the build embeds.
pub(crate) const AMBIGUOUS_CHAIN: &str = "; name one as `chain` in the request";
pub(crate) const AMBIGUOUS_CHAIN_LIST: &str = " share settlement on ";

/// The chains the connector listed in its ambiguous-chain refusal `text`, if it is one.
fn ambiguous_chains(text: &str) -> Option<&str> {
    let (before, _) = text.split_once(AMBIGUOUS_CHAIN)?;
    let (_, chains) = before.split_once(AMBIGUOUS_CHAIN_LIST)?;
    Some(chains.trim())
}

/// What the connector's answer to a peering is on the connector's own words: it confirmed
/// the opening deposit and then read no balance on the channel. The deposit is on chain, the
/// connector keeps its record of the channel, and the same request finds it. Pinned by a
/// test against the connector the build embeds.
pub(crate) const STALE_READ: &str = "confirmed, and the chain shows no balance there";

/// How many times a peering refused with [`STALE_READ`] is repeated, and how long to wait
/// before each. The wait is over a second so that no two requests are signed in the same
/// second, which the connector refuses as a replayed signature.
const STALE_READ_REPEATS: u32 = 3;
const STALE_READ_WAIT: Duration = Duration::from_millis(1_200);

/// What the connector's answer to a peering is when it could not read the other side's
/// self-description in time: the start of its refusal, which it has before any channel is
/// opened, and the reason a timed-out read gives. Nothing was deposited, and the connector
/// does not repeat the read, so this command does. A refusal for any other reason (no
/// proxy, a refused connection) has the first half of this and not the second. Pinned by
/// tests against the connector the build embeds.
pub(crate) const UNREAD: &str = "could not read the self-description at";
pub(crate) const UNREAD_TIMEOUT: &str = "operation timed out";

/// How many times a peering whose self-description read timed out is repeated. Each attempt
/// has taken the connector's 10 seconds, so no two are signed in the same second without
/// a pause.
const UNREAD_REPEATS: u32 = 3;

/// A peering made, and whether this command deposited into its channel.
pub struct Peered {
    pub report: Report,
    /// False when the channel was already open and nothing was deposited.
    pub deposited: bool,
}

/// `toon peer add`: create a peering toward the connector at `address`, opening and
/// funding the channel it pays on with `deposit`.
pub fn peer_add(home: &Path, add: &PeerAdd) -> Result<Peered, Error> {
    peer_add_on(&surface(home)?, add)
}

/// `peer_add`, on the connector whose operator surface is `surface`.
pub fn peer_add_on(surface: &Surface, add: &PeerAdd) -> Result<Peered, Error> {
    let id = add.id.map_or_else(|| label(add.address), str::to_owned);
    segment(ErrorCode::PeerFailed, "The peering's label", &id)?;
    let mut body = json!({
        "id": id,
        "url": add.address,
        "fee": add.fee,
        "max_packet_amount": add.max_packet_amount,
        "deposit": add.deposit,
    });
    if let (Some(chain), Some(members)) = (add.chain, body.as_object_mut()) {
        members.insert("chain".into(), json!(chain.name()));
    }
    let (mut status, mut text) = write(surface, reqwest::Method::POST, "/peers", Some(&body))?;
    // Set once the connector has said that the deposit this command sent confirmed.
    let mut deposit_confirmed = false;
    // However a repeat then fails, the deposit is on chain and the same command finds it.
    let unpeered = |why: &str| {
        failed(
            ErrorCode::PeerFailed,
            format!(
                "The deposit of {} confirmed on chain, but the peering was not created. \
                 Running the same command again finds the channel and deposits nothing \
                 more. {why}",
                add.deposit
            ),
        )
    };
    let (mut stale_repeats, mut unread_repeats) = (0, 0);
    loop {
        if status != 502 {
            break;
        }
        if text.contains(STALE_READ) && stale_repeats < STALE_READ_REPEATS {
            deposit_confirmed = true;
            stale_repeats += 1;
            std::thread::sleep(STALE_READ_WAIT);
        } else if text.contains(UNREAD)
            && text.contains(UNREAD_TIMEOUT)
            && unread_repeats < UNREAD_REPEATS
        {
            unread_repeats += 1;
        } else {
            break;
        }
        (status, text) =
            write(surface, reqwest::Method::POST, "/peers", Some(&body)).map_err(|error| {
                if deposit_confirmed {
                    unpeered(&error.message)
                } else {
                    error
                }
            })?;
    }
    if deposit_confirmed && status != 200 {
        return Err(unpeered(&refusal(status, &text)));
    }
    if status != 200 {
        if status == 400 {
            if let Some(chains) = ambiguous_chains(&text) {
                // Refused before any channel was opened: nothing was deposited.
                return Err(Error {
                    nothing_sent: true,
                    ..failed(
                        ErrorCode::PeerFailed,
                        format!(
                            "This connector and the other settle on more than one chain \
                             ({chains}). Make the peering again with `--chain` naming one. {}",
                            refusal(status, &text)
                        ),
                    )
                });
            }
        }
        // The connector reads the other side's self-description first, and a connector
        // that is not peerable publishes none a peer can use. A connector that does not
        // peer over plain `http://` refuses such an address, and finds no endpoint it
        // can dial in a description that publishes only those: then the refusal is
        // this side's.
        let no_endpoint = status == 502 && text.contains("publishes no endpoint");
        let no_client_edge = status == 502 && text.contains("publishes no httpEndpoint");
        let plaintext = status == 502 && text.contains("peer_allow_plaintext_endpoints");
        if !surface.plaintext_peers && (no_endpoint || plaintext) {
            return Err(failed(
                ErrorCode::PeerFailed,
                format!(
                    "This connector does not peer over plain `http://`. To peer on one \
                     machine, run `toon init` with `--allow-plaintext-peers`. {}",
                    refusal(status, &text)
                ),
            ));
        }
        return Err(if no_endpoint || no_client_edge {
            failed(
                ErrorCode::PeerNotPeerable,
                format!(
                    "{} is not peerable: the refusal is on the other side. {}",
                    add.address,
                    refusal(status, &text)
                ),
            )
        } else {
            surface.refused_write(ErrorCode::PeerFailed, status, &text)
        });
    }
    let peering: Value = serde_json::from_str(&text).map_err(|error| {
        failed(
            ErrorCode::PeerFailed,
            format!("The connector's answer was not understood ({error}): {text}"),
        )
    })?;
    let channel = peering["channel"]["id"].as_str().unwrap_or_default();
    let channel_status = peering["channel"]["status"].as_str().unwrap_or_default();
    // What the connector says it did decides what was deposited: a channel it found
    // moved nothing, unless this command's own deposit confirmed before a repeat found it.
    let deposited = deposit_confirmed || channel_status != "found";
    let paid = if deposited {
        format!("deposit {}", add.deposit)
    } else {
        "already open, nothing deposited".to_owned()
    };
    Ok(Peered {
        report: Report {
            exit: Exit::Success,
            json: json!({ "peering": peering, "deposited": deposited }),
            text: format!(
                "Peered with {} as {id}, paying on channel {channel} ({channel_status}), {paid}.\n\
                 Packets forwarded to it are served by that connector. It forwards back to you \
                 only if its operator creates a peering toward you in return.",
                add.address
            ),
        },
        deposited,
    })
}

/// [`peer_add_on`], then forward `prefix` over the peering. A deposit the peering made is
/// added to `deposits` before the route is asked for, so a route that then fails, even
/// before anything is sent, does not give that deposit back.
pub fn peer_and_route_on(
    surface: &Surface,
    add: &PeerAdd,
    prefix: &str,
    deposits: &mut u128,
) -> Result<Peered, Error> {
    let id = add.id.map_or_else(|| label(add.address), str::to_owned);
    let peered = peer_add_on(surface, add)?;
    *deposits += u128::from(peered.deposited);
    route_add_on(surface, prefix, &id, 0)?;
    Ok(peered)
}

/// `toon peer list`: the connector's peerings.
pub fn peer_list(home: &Path) -> Result<Report, Error> {
    let surface = surface(home)?;
    let peers = read(&surface, "/peers")?;
    let mut lines: Vec<String> = peers
        .iter()
        .map(|peer| {
            format!(
                "{} (fee {}, cap {})",
                peer["id"].as_str().unwrap_or_default(),
                peer["fee"],
                peer["max_packet_amount"]
            )
        })
        .collect();
    if lines.is_empty() {
        lines.push("The connector has no peerings.".into());
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "peers": peers }),
        text: lines.join("\n"),
    })
}

/// `segment`, checked to be one path segment the connector reads as written: a peering's
/// label or an ILP address prefix, which hold nothing a URL would escape.
fn segment(code: ErrorCode, what: &str, segment: &str) -> Result<(), Error> {
    if !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '~'))
    {
        return Ok(());
    }
    Err(failed(
        code,
        format!("{what} {segment:?} holds a character other than letters, digits, `.`, `-`, `_` and `~`."),
    ))
}

/// `toon peer remove`: remove the peering called `id`.
pub fn peer_remove(home: &Path, id: &str) -> Result<Report, Error> {
    segment(ErrorCode::PeerFailed, "The peering's label", id)?;
    let surface = surface(home)?;
    let (status, text) = write(
        &surface,
        reqwest::Method::DELETE,
        &format!("/peers/{id}"),
        None,
    )?;
    if status != 204 {
        return Err(refused(ErrorCode::PeerFailed, status, &text));
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "removed": id }),
        text: format!("Removed the peering {id}."),
    })
}

/// `toon route add`: forward packets for `prefix` to the peering `peer`, at `price`.
pub fn route_add(home: &Path, prefix: &str, peer: &str, price: u64) -> Result<Report, Error> {
    route_add_on(&surface(home)?, prefix, peer, price)
}

/// `route_add`, on the connector whose operator surface is `surface`.
pub fn route_add_on(
    surface: &Surface,
    prefix: &str,
    peer: &str,
    price: u64,
) -> Result<Report, Error> {
    let body = json!({ "prefix": prefix, "peer_id": peer, "price": price });
    let (status, text) = write(surface, reqwest::Method::POST, "/routes/peers", Some(&body))?;
    if status != 200 {
        return Err(refused(ErrorCode::RouteFailed, status, &text));
    }
    let route: Value = serde_json::from_str(&text).map_err(|error| {
        failed(
            ErrorCode::RouteFailed,
            format!("The connector's answer was not understood ({error}): {text}"),
        )
    })?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "route": route }),
        text: format!("Forwarding {prefix} to {peer} (price {price})."),
    })
}

/// `toon route remove`: stop forwarding `prefix`.
pub fn route_remove(home: &Path, prefix: &str) -> Result<Report, Error> {
    segment(ErrorCode::RouteFailed, "The prefix", prefix)?;
    let surface = surface(home)?;
    let (status, text) = write(
        &surface,
        reqwest::Method::DELETE,
        &format!("/routes/peers/{prefix}"),
        None,
    )?;
    if status != 204 {
        return Err(refused(ErrorCode::RouteFailed, status, &text));
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "removed": prefix }),
        text: format!("Stopped forwarding {prefix}."),
    })
}

/// The wallet's operator write key, the 32 bytes `init` wrote, as the keypair the
/// connector's request signing takes.
fn write_keypair(path: &Path) -> Result<ed25519_dalek_v1::Keypair, Error> {
    let unusable = |reason: String| failed(ErrorCode::Io, format!("{}: {reason}.", path.display()));
    let raw = Zeroizing::new(std::fs::read(path).map_err(|source| unusable(source.to_string()))?);
    let secret = ed25519_dalek_v1::SecretKey::from_bytes(&raw)
        .map_err(|_| unusable("the operator write key is not 32 bytes".into()))?;
    let public = ed25519_dalek_v1::PublicKey::from(&secret);
    Ok(ed25519_dalek_v1::Keypair { secret, public })
}

/// A channel write: `POST path` with `body`, JSON or empty, signed with the wallet's
/// operator write key. The connector's answer is the channel after the write.
///
/// The body is JSON text rather than a `Value`, because an amount is a `u128` and a
/// `Value` holds no number above `u64::MAX`.
fn channel_write(home: &Path, path: &str, body: String) -> Result<Value, Error> {
    let surface = surface(home)?;
    let url = format!("{}{path}", surface.url);
    let channel_failed = |message: String| failed(ErrorCode::ChannelFailed, message);
    let (status, text) = write_text(&surface, reqwest::Method::POST, path, body, PATIENCE)
        .map_err(|error| Error {
            code: match error.code {
                ErrorCode::ConnectorFailed => ErrorCode::ChannelFailed,
                other => other,
            },
            ..error
        })?;
    if !(200..300).contains(&status) {
        if let Some(unfunded) = surface.refused_for_gas(status, &text) {
            return Err(unfunded);
        }
        return Err(Error {
            // The connector authenticates a write before it does anything.
            nothing_sent: status == 401,
            ..channel_failed(format!("POST {url} answered {status}: {}", text.trim()))
        });
    }
    serde_json::from_str(&text).map_err(|error| {
        channel_failed(format!(
            "POST {url}: the answer was not understood ({error}): {text}"
        ))
    })
}

/// A channel id goes into the path the write is signed for, so it is only ever hex or base58.
fn channel_path(id: &str, action: &str) -> Result<String, Error> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(failed(
            ErrorCode::ChannelFailed,
            format!("'{id}' is not a channel id."),
        ));
    }
    Ok(format!("/channels/{id}/{action}"))
}

fn channel_report(channel: Value, headline: &str) -> Report {
    Report {
        exit: Exit::Success,
        text: format!("{headline}: {}", describe_channel(&channel)),
        json: json!({ "channel": channel }),
    }
}

/// `toon channel open`: open an outbound channel toward the counterparty whose terms
/// `terms` holds, with `deposit` in it.
pub fn channel_open(
    home: &Path,
    terms: &Path,
    deposit: u128,
    url: Option<&str>,
) -> Result<Report, Error> {
    ensure_gas(home)?;
    let contents = std::fs::read_to_string(terms).map_err(|source| {
        failed(
            ErrorCode::ChannelFailed,
            format!("{}: {source}.", terms.display()),
        )
    })?;
    let terms: Value = serde_json::from_str(&contents).map_err(|source| {
        failed(
            ErrorCode::ChannelFailed,
            format!("{}: the terms are not JSON ({source}).", terms.display()),
        )
    })?;
    let url = url.map_or(String::new(), |url| format!(r#","url":{}"#, json!(url)));
    let channel = channel_write(
        home,
        "/channels",
        format!(r#"{{"terms":{terms},"deposit":{deposit}{url}}}"#),
    )?;
    let headline = if channel["resumed"] == true {
        "Resumed an open"
    } else {
        "Opened"
    };
    Ok(channel_report(channel, headline))
}

/// `toon channel fund`: add `amount` to an outbound channel.
pub fn channel_fund(home: &Path, id: &str, amount: u128) -> Result<Report, Error> {
    ensure_gas(home)?;
    let channel = channel_write(
        home,
        &channel_path(id, "fund")?,
        format!(r#"{{"amount":{amount}}}"#),
    )?;
    Ok(channel_report(channel, &format!("Funded with {amount}")))
}

/// `toon channel withdraw`: the connector starts the withdrawal, or finishes it once it
/// is due, and says which.
pub fn channel_withdraw(home: &Path, id: &str) -> Result<Report, Error> {
    ensure_gas(home)?;
    let channel = channel_write(home, &channel_path(id, "withdraw")?, String::new())?;
    let step = channel["step"].as_str().unwrap_or("withdraw").to_owned();
    Ok(channel_report(channel, &format!("Withdrawal step {step}")))
}

/// `toon channel land`: land the latest voucher held on an inbound channel.
pub fn channel_land(home: &Path, id: &str) -> Result<Report, Error> {
    ensure_gas(home)?;
    let channel = channel_write(home, &channel_path(id, "land")?, String::new())?;
    Ok(channel_report(channel, "Landed"))
}

/// Refuse with `unfunded` unless the settlement key of the TOON app these commands are
/// about holds the gas a transaction spends. Nothing has been charged or sent yet.
fn ensure_gas(home: &Path) -> Result<(), Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let app = crate::apps::toon_app(&state, TARGET.get().map(String::as_str))?;
    crate::funding::ensure_gas(home, state.network, app)
}

/// What a packet came to.
pub enum Answer {
    Fulfilled {
        status: u64,
        body: String,
    },
    Rejected {
        code: String,
        message: String,
        /// The accumulated cost the connector stated, in base units; 0 when it stated none.
        cost: u128,
    },
    WrongFulfilment,
    /// The connector did not answer within [`packet_wait`]. The packet has expired by now.
    Unanswered,
}

/// How long a packet sent with [`dispatch_with_headers`] lives before a connector must
/// reject it.
const PACKET_EXPIRY: Duration = Duration::from_secs(30);

/// How long the command line waits for its connector's answer to a packet: the packet's
/// expiry and a few seconds more, so that a packet that outlasts its expiry is answered
/// with the connector's own reject rather than given up on. The hidden
/// `TOON_PACKET_WAIT_MS` shortens it for a test of a wait that runs out; it is not
/// documented and an operator is not told of it.
fn packet_wait() -> Duration {
    std::env::var("TOON_PACKET_WAIT_MS")
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map_or(
            PACKET_EXPIRY + Duration::from_secs(5),
            Duration::from_millis,
        )
}

fn spoken(wait: Duration) -> String {
    if wait.subsec_millis() != 0 {
        return format!("{} milliseconds", wait.as_millis());
    }
    match wait.as_secs() {
        1 => "1 second".to_owned(),
        seconds => format!("{seconds} seconds"),
    }
}

/// What a packet nobody answered within the wait came to, for the operator.
pub fn unanswered_message() -> String {
    format!(
        "The connector did not answer within {}. The packet has expired by now and will not \
         be delivered; the command can be run again.",
        spoken(packet_wait())
    )
}

/// `error`, if it is for a packet that went unanswered, with the cost the caller read from
/// the watermarks in place of the whole amount [`send`] and the like assume.
pub fn repriced(error: Error, paid: u128) -> Error {
    match error.unanswered {
        Some(unanswered) => unanswered_cost(paid, unanswered.event),
        None => error,
    }
}

/// The error for a packet that went unanswered, with what it cost and, if it carried one,
/// the event, whose id the text names so that the relay can be asked for it.
pub fn unanswered_cost(paid: u128, event: Option<Value>) -> Error {
    let sentence = if paid > 0 {
        format!(" It cost {paid} base units.")
    } else {
        String::new()
    };
    let id = event
        .as_ref()
        .and_then(|event| event["id"].as_str())
        .map(|id| format!(" Event {id} may be asked of the relay with `toon event query`."))
        .unwrap_or_default();
    let message = format!("{}{sentence}{id}", unanswered_message());
    Error {
        unanswered: Some(crate::outcome::Unanswered { paid, event }),
        ..failed(ErrorCode::SendFailed, message)
    }
}

/// Read the connector's summary of a send. The connector's own `send` verb is
/// what forms, seals, signs and checks the packet, and it reports in text.
fn answer(summary: &str) -> Option<Answer> {
    if summary.starts_with("FULFILL WITH THE WRONG FULFILMENT") {
        return Some(Answer::WrongFulfilment);
    }
    if summary.starts_with("FULFILL -- ") {
        let (_, answered) = summary.split_once("the terminating app answered ")?;
        let (status, body) = answered.split_once(":\n")?;
        return Some(Answer::Fulfilled {
            status: status.parse().ok()?,
            body: body.to_owned(),
        });
    }
    let rejected = summary.strip_prefix("REJECT ")?;
    let (code, rest) = rejected.split_once(" -- ")?;
    let message = rest.split_once('\n').map_or("", |(_, message)| message);
    // The summary ends with `accumulated cost: N base units`, which is not the reject's message.
    let (message, cost) = match message.rsplit_once('\n') {
        Some((message, last)) if accumulated_cost(last).is_some() => {
            (message, accumulated_cost(last))
        }
        None if accumulated_cost(message).is_some() => ("", accumulated_cost(message)),
        _ => (message, None),
    };
    Some(Answer::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
        cost: cost.unwrap_or(0),
    })
}

/// The cost in `line`, if it is the summary's last line of a reject, the cost of its path.
fn accumulated_cost(line: &str) -> Option<u128> {
    line.strip_prefix("accumulated cost: ")?
        .strip_suffix(" base units")?
        .parse()
        .ok()
}

/// Add the cost of a rejected packet to its report's `json`, beside `outcome`: `cost` as a
/// decimal string and `complete`, which is `false` for an `R01`, whose cost is the amount to
/// carry past the connector that stopped the packet. A cost of 0 adds nothing.
pub fn add_cost(json: &mut Value, code: &str, cost: u128) {
    if cost > 0 {
        json["cost"] = json!(cost.to_string());
        json["complete"] = json!(code != "R01");
    }
}

/// The sentence that states a rejected packet's `cost`, or nothing for a cost of 0. `flag` is
/// the option that states an amount: the whole cost for a complete one, or, for an `R01`, the
/// amount to carry past the connector that stopped the packet.
pub fn cost_sentence(code: &str, cost: u128, flag: &str) -> String {
    if cost == 0 {
        String::new()
    } else if code == "R01" {
        format!(
            " The packet stopped at a connector it could not pay: {cost} base units is the amount \
             to carry to get past it, not the whole cost."
        )
    } else {
        format!(" The path costs {cost} base units: state `{flag} {cost}`.")
    }
}

/// Whether the `Debug` of the connector's `SendError` is its `Transport` variant for a
/// packet write answered 401, which is worded `401 Unauthorized -- ...`. The connector
/// authenticates the write before it forms a packet, so none was sent.
fn refused_with_401(variant: &str) -> bool {
    variant.starts_with("Transport") && variant.contains("reason: \"401 ")
}

/// Whether the connector's `send` failed before it wrote to the operator surface: its
/// arguments were refused, the operator key could not be read, the identity to seal to
/// could not be fetched, the packet could not be sealed, or the write was refused with 401. Any
/// other `Transport` failure may come after the packet left, and an answer not understood
/// does, so neither is here.
///
/// The connector keeps `SendError` private, so its variant is read from its `Debug`.
fn before_sending(error: &connector_cli::CliError) -> bool {
    match error {
        connector_cli::CliError::Usage(_) => true,
        connector_cli::CliError::Send(send) => {
            let variant = format!("{send:?}");
            ["KeyFile", "Identity", "Seal"]
                .iter()
                .any(|name| variant.starts_with(name))
                || refused_with_401(&variant)
        }
        _ => false,
    }
}

/// Send one packet from the operator surface to `destination`, for `amount`, sealed to the
/// connector at `seal_to`, or to this one, and return what the connector says it came to.
/// `method` and `target` are the request the app receives; `body` is a file the request
/// carries as its JSON body.
pub fn dispatch(
    home: &Path,
    destination: &str,
    amount: u64,
    seal_to: Option<&str>,
    method: &str,
    target: &str,
    body: Option<&Path>,
) -> Result<Answer, Error> {
    let surface = surface(home)?;
    let send_failed = |message: String| failed(ErrorCode::SendFailed, message);
    // A body that cannot be read is found out before anything is sent.
    if let Some(path) = body {
        std::fs::read(path).map_err(|error| Error {
            nothing_sent: true,
            ..send_failed(format!("{} could not be read: {error}.", path.display()))
        })?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| send_failed(format!("The async runtime did not start: {error}.")))?;
    let amount_text = amount.to_string();
    let seal_to = seal_to.map_or_else(|| format!("{}/ilp", surface.url), str::to_owned);
    let write_key = surface.write_key.to_string_lossy();
    let body = body.map(|path| path.to_string_lossy().into_owned());
    let egress = Egress::of(home)?;
    // An overlay that cannot be had is found out before anything is sent.
    let socks_proxy = egress
        .proxy_for(&seal_to)
        .map_err(|error| Error {
            nothing_sent: true,
            ..error
        })?
        .map(|proxy| format!("socks5h://{proxy}"));
    // The connector's `send` takes its proxy only to an onion endpoint and dials any other
    // host directly, so a request that must go through the overlay to another host is
    // formed here, with the same envelope the connector's `send` makes.
    if socks_proxy.is_some() && !is_onion_endpoint(&seal_to) {
        let body = match &body {
            Some(path) => std::fs::read(path).map_err(|error| Error {
                nothing_sent: true,
                ..send_failed(format!("{path} could not be read: {error}."))
            })?,
            None => Vec::new(),
        };
        let headers = vec![("content-type".to_owned(), "application/json".to_owned())];
        let public = identity(&egress, &seal_to)?;
        return dispatch_with_headers(
            home,
            destination,
            amount,
            &public,
            (method, target),
            headers,
            body,
        );
    }
    let mut arguments = vec![
        "toon send",
        "send",
        "--operator",
        &surface.url,
        "--operator-key",
        &write_key,
        "--to",
        destination,
        "--seal-to",
        &seal_to,
        "--amount",
        &amount_text,
        "--method",
        method,
        "--target",
        target,
    ];
    if let Some(body) = &body {
        arguments.extend(["--body", body]);
    }
    if let Some(proxy) = &socks_proxy {
        arguments.extend(["--socks-proxy", proxy]);
    }
    let summary = runtime
        .block_on(connector_cli::run(&arguments))
        .map_err(|error| {
            let mut failure = send_failed(error.to_string());
            failure.nothing_sent = before_sending(&error);
            failure
        })
        .and_then(|command| match command {
            connector_cli::Command::Finished { summary } => Ok(summary),
            connector_cli::Command::Serve(_) => {
                Err(send_failed("The connector did not send.".into()))
            }
        })?;
    answer(&summary).ok_or_else(|| {
        send_failed(format!(
            "The connector's answer was not understood: {summary}"
        ))
    })
}

/// Whether `url` names an onion endpoint, which the connector's `send` dials through its
/// `--socks-proxy`.
fn is_onion_endpoint(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        url.host_str().is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            host.ends_with(&format!(".{}", crate::overlay::TLD)) || host.ends_with(".onion")
        })
    })
}

/// The sealing key the connector at `seal_to` gives as its identity, asked for the way
/// `egress` says a request leaves this machine. Nothing has been sent when it fails.
fn identity(egress: &Egress, seal_to: &str) -> Result<[u8; 65], Error> {
    let not_sent = |message: String| Error {
        nothing_sent: true,
        ..failed(ErrorCode::SendFailed, message)
    };
    let identity_url = format!("{}/identity", seal_to.trim_end_matches('/'));
    let identity: Value = egress
        .client(&identity_url, PATIENCE)
        .map_err(|error| Error {
            nothing_sent: true,
            ..error
        })?
        .get(&identity_url)
        .send()
        .and_then(|response| response.json())
        .map_err(|error| {
            not_sent(format!(
                "{identity_url} did not give its identity: {error}."
            ))
        })?;
    identity["publicKey"]
        .as_str()
        .and_then(|key| hex::decode(key.trim_start_matches("0x")).ok())
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| not_sent(format!("{identity_url} has no 65-byte `publicKey`.")))
}

/// Send one packet like [`dispatch`] does, but sealed to the key `public` itself, which
/// is not fetched from anywhere, with the request's `(method, target)`, `headers` on the
/// request and `body` in memory. The connector's `send` fixes the request's headers, so this
/// forms, seals and signs the packet itself, with the connector's own crates, and reads the
/// answer the same way.
pub fn dispatch_with_headers(
    home: &Path,
    destination: &str,
    amount: u64,
    public: &[u8; 65],
    (method, target): (&str, &str),
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<Answer, Error> {
    use connector_domain::{EnvelopeRequest, EnvelopeResponse, Fulfill, Prepare, Reject};
    use connector_signer::giftwrap::{derive_fulfillment, open_response, seal_request};

    let surface = surface(home)?;
    let send_failed = |message: String| failed(ErrorCode::SendFailed, message);
    let not_sent = |message: String| Error {
        nothing_sent: true,
        ..send_failed(message)
    };
    let keypair = write_keypair(&surface.write_key).map_err(|error| Error {
        nothing_sent: true,
        ..error
    })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(packet_wait())
        .build()
        .map_err(|error| not_sent(error.to_string()))?;

    let plaintext = EnvelopeRequest {
        method: method.into(),
        target: target.into(),
        headers,
        body,
    }
    .encode();
    let (data, secret) = seal_request(&plaintext, public)
        .map_err(|error| not_sent(format!("The packet could not be sealed: {error}.")))?;
    let prepare = Prepare {
        amount,
        expires_at: chrono::Utc::now()
            + chrono::Duration::from_std(PACKET_EXPIRY).unwrap_or_default(),
        greeting: false,
        destination: destination.to_owned(),
        data,
    }
    .encode();
    let created = chrono::Utc::now().timestamp().max(0) as u64;
    let (signature_input, signature, content_digest) = connector_operator::signing::sign_request(
        &keypair,
        "POST",
        "/packets",
        &prepare,
        created,
        Some(created + 60),
    );
    // A wait that ran out is an answer of its own, not a failure: the packet may have been
    // paid for.
    let response = match client
        .post(format!("{}/packets", surface.url))
        .header("content-type", "application/octet-stream")
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature)
        .body(prepare)
        .send()
    {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Ok(Answer::Unanswered),
        Err(error) => return Err(send_failed(error.to_string())),
    };
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) if error.is_timeout() => return Ok(Answer::Unanswered),
        Err(error) => return Err(send_failed(error.to_string())),
    };
    if !status.is_success() {
        return Err(Error {
            // The write is refused before any packet is formed.
            nothing_sent: status == reqwest::StatusCode::UNAUTHORIZED,
            ..send_failed(format!(
                "The connector refused the write with {status}: {}",
                String::from_utf8_lossy(&bytes).trim()
            ))
        });
    }
    let undecodable = |reason: String| {
        send_failed(format!(
            "The connector's answer was not understood: {reason}"
        ))
    };
    match Fulfill::decode(&bytes) {
        Ok(fulfill) if fulfill.fulfillment != derive_fulfillment(&secret) => {
            Ok(Answer::WrongFulfilment)
        }
        Ok(fulfill) => {
            let opened = open_response(&secret, &fulfill.data)
                .map_err(|error| undecodable(error.to_string()))?;
            let envelope = EnvelopeResponse::decode(&opened)
                .map_err(|error| undecodable(error.to_string()))?;
            Ok(Answer::Fulfilled {
                status: envelope.status.into(),
                body: String::from_utf8_lossy(&envelope.body).into_owned(),
            })
        }
        Err(_) => {
            let reject = Reject::decode(&bytes).map_err(|error| undecodable(error.to_string()))?;
            // An absent or unreadable header is a cost of 0.
            let cost = headers
                .get("TOON-Accumulated-Cost")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            Ok(Answer::Rejected {
                code: reject.code.as_str().to_owned(),
                message: reject.message,
                cost,
            })
        }
    }
}

/// `toon send`: one packet from the operator surface to `destination`, for `amount`,
/// sealed to the connector at `seal_to`, or to this one.
/// A packet that is not fulfilled is a report, not an error, and exits 1.
pub fn send(
    home: &Path,
    destination: &str,
    amount: u64,
    seal_to: Option<&str>,
    method: &str,
    target: &str,
    body: Option<&Path>,
) -> Result<Report, Error> {
    // The connector takes the target relative to the route's handler, and `--path` names
    // it from the handler's root.
    let target = match target.strip_prefix('/') {
        Some("") | None => target,
        Some(relative) => relative,
    };
    let answer = dispatch(home, destination, amount, seal_to, method, target, body)?;
    if matches!(answer, Answer::Unanswered) {
        // `toon send` reads the watermarks, which only its caller can.
        return Err(unanswered_cost(u128::from(amount), None));
    }
    let sent = format!("{amount} base units to {destination}");
    Ok(match answer {
        Answer::Fulfilled { status, body } => Report {
            exit: Exit::Success,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "fulfilled",
                "response": { "status": status, "body": body },
            }),
            text: format!("Fulfilled: {sent}. The app answered {status}."),
        },
        Answer::Rejected {
            code,
            message,
            cost,
        } => {
            let mut json = json!({
                "destination": destination,
                "amount": amount,
                "outcome": "rejected",
                "reject": { "code": code, "message": message },
            });
            add_cost(&mut json, &code, cost);
            Report {
                exit: Exit::Failure,
                json,
                text: format!(
                    "Rejected with {code}: {sent}. {message}{}",
                    cost_sentence(&code, cost, "--amount")
                ),
            }
        }
        Answer::Unanswered => unreachable!("handled above"),
        Answer::WrongFulfilment => Report {
            exit: Exit::Failure,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "wrong_fulfilment",
            }),
            text: format!(
                "Fulfilled, but not by this connector: {sent}. The fulfilment did not match."
            ),
        },
    })
}

/// `toon channel`.
pub fn channel(home: &Path, command: ChannelCommand) -> Result<Report, Error> {
    match command {
        ChannelCommand::List => channel_list(home),
        ChannelCommand::Open {
            terms,
            deposit,
            url,
        } => channel_open(home, &terms, deposit, url.as_deref()),
        ChannelCommand::Fund { id, amount } => channel_fund(home, &id, amount),
        ChannelCommand::Withdraw { id } => channel_withdraw(home, &id),
        ChannelCommand::Land { id } => channel_land(home, &id),
    }
}

/// `toon peer`.
pub fn peer(home: &Path, command: &PeerCommand) -> Result<Report, Error> {
    match command {
        PeerCommand::Add(args) => {
            // Refused before the spending limit is charged: nothing is sent.
            ensure_gas(home)?;
            spending::spend(home, args.deposit, args.yes, || {
                let peered = peer_add(
                    home,
                    &PeerAdd {
                        address: &args.address,
                        deposit: args.deposit,
                        id: args.id.as_deref(),
                        fee: args.fee,
                        max_packet_amount: args.max_packet_amount,
                        chain: args.chain,
                    },
                )?;
                Ok((peered.report, peered.deposited))
            })
        }
        PeerCommand::List => peer_list(home),
        PeerCommand::Remove { id } => peer_remove(home, id),
    }
}

/// `toon route`.
pub fn route(home: &Path, command: &RouteCommand) -> Result<Report, Error> {
    match command {
        RouteCommand::List => route_list(home),
        RouteCommand::Add {
            prefix,
            peer,
            price,
        } => route_add(home, prefix, peer, *price),
        RouteCommand::Price { prefix, price, yes } => {
            crate::apps::route_price(home, prefix, *price, *yes)
        }
        RouteCommand::Remove { prefix } => route_remove(home, prefix),
    }
}

/// The prefix a joined network's packets are forwarded under.
const NETWORK_PREFIX: &str = "g.toon";

/// `toon join`: peer toward the network's connector for `deposit`, forward the network's
/// prefix over the peering, and read the network's relay. All of it is under the
/// spending limit, as the one deposit.
pub fn join(home: &Path, args: &JoinArgs) -> Result<Report, Error> {
    // Refused before the spending limit is charged: a refused join moves nothing.
    let Some(mut state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let name = args.network.name();
    if state.network != args.network {
        return Err(failed(
            ErrorCode::JoinRefused,
            format!(
                "This agent node was initialised for {}, whose chain it settles on: it cannot join {name}.",
                state.network.name()
            ),
        ));
    }
    if let Some(joined) = &state.joined {
        return Err(failed(
            ErrorCode::JoinRefused,
            format!("This agent node has already joined {joined}."),
        ));
    }
    let Some(connector_url) = state.connector_url.clone() else {
        if state.network == Profile::Sandbox {
            return Err(failed(
                ErrorCode::JoinRefused,
                crate::wallet::SANDBOX_HUB_NOTE.to_owned(),
            ));
        }
        return Err(failed(
            ErrorCode::JoinRefused,
            format!(
                "There is no {name} TOON network yet: this agent node records no connector for it. \
                 Name one with `--connector-url` (and `--relay-url`) on `init`."
            ),
        ));
    };
    let app = crate::apps::toon_app(&state, TARGET.get().map(String::as_str))?;
    crate::funding::ensure_gas(home, state.network, app)?;
    spending::spend(home, args.deposit, args.yes, || {
        let peered = peer_add(
            home,
            &PeerAdd {
                address: &connector_url,
                deposit: args.deposit,
                id: Some(name),
                fee: 0,
                max_packet_amount: 0,
                chain: args.chain,
            },
        )?;
        let deposited = peered.deposited;
        let peered = peered.report;
        let routed = route_add(home, NETWORK_PREFIX, name, 0)?;
        let relay = state.relay_url.clone();
        state.joined = Some(name.to_owned());
        if let Some(relay) = &relay {
            if !state.reads.contains(relay) {
                state.reads.push(relay.clone());
            }
        }
        state.save(home)?;
        let paid = if deposited {
            format!("deposit {}", args.deposit)
        } else {
            "its channel already open and nothing deposited".to_owned()
        };
        let reading = match &relay {
            Some(relay) => format!("Reading its relay at {relay}."),
            None => "It names no relay, so the agent reads no relay of it.".to_owned(),
        };
        Ok((
            Report {
                exit: Exit::Success,
                json: json!({
                    "network": name,
                    "peering": peered.json["peering"],
                    "route": routed.json["route"],
                    "relay": relay,
                    "deposited": deposited,
                }),
                text: format!(
                    "Joined {name}: peered with {connector_url} and forwarding {NETWORK_PREFIX} to it, {paid}. \
                     {reading}\n\
                     Its connector forwards back to you only if its operator creates a peering toward you in return.",
                ),
            },
            deposited,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        answer, before_sending, packet_wait, peer_add_on, peer_and_route_on, Answer, PeerAdd,
        Surface, AMBIGUOUS_CHAIN, AMBIGUOUS_CHAIN_LIST, PACKET_EXPIRY, REPLAYED, STALE_READ,
        UNREAD, UNREAD_TIMEOUT,
    };
    use crate::cli::Chain;
    use crate::outcome::ErrorCode;
    use std::io::{Read, Write};

    /// A connector that answers each `POST /peers` with the next of `answers`, and says
    /// when each arrived. The last answer repeats.
    fn connector(
        answers: Vec<(u16, String)>,
    ) -> (String, std::sync::mpsc::Receiver<std::time::Instant>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let (arrived, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                let mut stream = stream.expect("a connection");
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                let _ = arrived.send(std::time::Instant::now());
                let (status, body) = &answers[index.min(answers.len() - 1)];
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, seen)
    }

    fn surface(home: &std::path::Path, url: String) -> Surface {
        let write_key = home.join("write.key");
        std::fs::write(&write_key, [7u8; 32]).expect("the key");
        Surface {
            url,
            bearer_token: home.join("token"),
            write_key,
            plaintext_peers: true,
            gas: None,
        }
    }

    fn add() -> PeerAdd<'static> {
        PeerAdd {
            address: "http://127.0.0.1:1/ilp",
            deposit: 5,
            id: Some("far"),
            fee: 0,
            max_packet_amount: 0,
            chain: None,
        }
    }

    fn stale() -> (u16, String) {
        (502, format!("the opening deposit into 'c' {STALE_READ}"))
    }

    fn unread() -> (u16, String) {
        (
            502,
            format!("{UNREAD} http://x.anyone/ilp: error sending request: {UNREAD_TIMEOUT}"),
        )
    }

    fn found() -> (u16, String) {
        (
            200,
            r#"{"id":"far","channel":{"id":"c","status":"found"}}"#.to_owned(),
        )
    }

    #[test]
    fn a_peering_refused_for_a_stale_read_is_repeated_a_second_apart_and_counts_the_deposit() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![stale(), stale(), found()]);
        let peered = peer_add_on(&surface(home.path(), url), &add()).expect("peered");
        assert!(peered.deposited, "the deposit this command sent is counted");
        assert!(!peered.report.text.contains("nothing deposited"));
        let times: Vec<_> = seen.try_iter().collect();
        assert_eq!(times.len(), 3);
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn a_502_about_gas_is_unfunded_and_any_other_stays_peer_failed_with_its_text() {
        let home = tempfile::tempdir().expect("a directory");
        let gas = (502, "out of gas: gas required exceeds: 66555".to_owned());
        let other = (502, "the peer refused the handshake".to_owned());
        for ((status, text), code) in [(gas, ErrorCode::Unfunded), (other, ErrorCode::PeerFailed)] {
            let (url, _) = connector(vec![(status, text.clone())]);
            let surface = Surface {
                gas: Some(super::GasRefusal::example()),
                ..surface(home.path(), url)
            };
            let Err(error) = peer_add_on(&surface, &add()) else {
                panic!("the peering was made");
            };
            assert_eq!(error.code, code, "{}", error.message);
            if code == ErrorCode::PeerFailed {
                assert_eq!(error.message, super::refusal(status, &text));
            } else {
                assert!(error.message.contains("0xabc"), "{}", error.message);
            }
        }
    }

    #[test]
    fn a_peering_refused_every_time_fails_and_says_to_run_it_again() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![stale()]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(
            error.message.contains("confirmed on chain"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("same command again"),
            "{}",
            error.message
        );
        assert!(!crate::spending::failed_before_paying(&error));
        assert_eq!(
            seen.try_iter().count(),
            1 + super::STALE_READ_REPEATS as usize
        );
    }

    #[test]
    fn a_rejects_message_leaves_out_the_accumulated_cost_line() {
        let summary =
            "REJECT F02 -- no route\nNo route to g.nobody.here.\naccumulated cost: 101 base units";
        match answer(summary) {
            Some(Answer::Rejected {
                code,
                message,
                cost,
            }) => {
                assert_eq!(code, "F02");
                assert_eq!(message, "No route to g.nobody.here.");
                assert_eq!(cost, 101);
            }
            _ => panic!("not a reject"),
        }
        match answer("REJECT F02 -- no route\naccumulated cost: 0 base units") {
            Some(Answer::Rejected { message, .. }) => assert_eq!(message, ""),
            _ => panic!("not a reject"),
        }
    }

    #[test]
    fn a_channel_found_on_the_first_answer_moved_nothing() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, _) = connector(vec![found()]);
        let peered = peer_add_on(&surface(home.path(), url), &add()).expect("peered");
        assert!(!peered.deposited);
        assert!(peered.report.text.contains("nothing deposited"));
        assert!(!peered.report.text.contains("deposit 5"));
        assert_eq!(peered.report.json["deposited"], false);
    }

    #[test]
    fn a_repeat_refused_another_way_still_says_the_deposit_confirmed() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, _) = connector(vec![
            stale(),
            (401, "signature has already been used".to_owned()),
        ]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(
            error.message.contains("confirmed on chain"),
            "{}",
            error.message
        );
        assert!(!crate::spending::failed_before_paying(&error));
    }

    fn replayed() -> (u16, String) {
        (401, super::REPLAYED.to_owned())
    }

    #[test]
    fn a_write_refused_as_a_replay_is_signed_again_once_the_second_has_turned() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![replayed(), found()]);
        let peered = peer_add_on(&surface(home.path(), url), &add()).expect("peered");
        assert!(!peered.deposited, "the connector found the channel");
        let times: Vec<_> = seen.try_iter().collect();
        assert_eq!(times.len(), 2);
        let wait = times[1] - times[0];
        assert!(wait < std::time::Duration::from_millis(1_500), "{wait:?}");
    }

    #[test]
    fn a_write_refused_as_a_replay_twice_says_nothing_was_done_and_is_not_repeated_again() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![replayed()]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(
            error.message.contains("Nothing was done"),
            "{}",
            error.message
        );
        assert!(error.message.contains("again"), "{}", error.message);
        assert!(crate::spending::failed_before_paying(&error));
        assert_eq!(seen.try_iter().count(), 2);
    }

    #[test]
    fn a_write_refused_with_401_for_another_reason_is_not_repeated_and_paid_nothing() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![(401, "signature has expired".to_owned())]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert_eq!(
            error.message,
            "The connector answered 401: signature has expired"
        );
        assert!(crate::spending::failed_before_paying(&error));
        assert_eq!(seen.try_iter().count(), 1);
    }

    /// The repeat of a peering is keyed on the connector's wording, which the connector
    /// keeps in a string literal after the channel's quoted name. The connector's crates
    /// are linked into this binary, so a pin move that rewords the refusal fails here
    /// instead of ending the repeat. `STALE_READ` alone is in this binary whatever the
    /// connector says, so the search is for it as the connector's literal goes on.
    #[test]
    fn the_connectors_stale_read_wording_is_pinned() {
        let binary = std::fs::read(std::env::current_exe().expect("this binary")).expect("read");
        let wording = [b"' ".as_slice(), STALE_READ.as_bytes()].concat();
        let found = binary
            .windows(wording.len())
            .any(|window| window == wording.as_slice());
        assert!(
            found,
            "the embedded connector no longer says {STALE_READ:?}"
        );
    }

    /// The timeout repeat is keyed on the start of the connector's refusal, a string literal
    /// in its crates, and on the reason reqwest gives a read that outlasted its wait.
    /// `tests/peer_unread.rs` pins the pair through the running connector.
    #[test]
    fn the_connectors_unread_wording_is_pinned() {
        let binary = std::fs::read(std::env::current_exe().expect("this binary")).expect("read");
        // A format string is kept in pieces, so the literal ends where `{url}` begins.
        let wording = [UNREAD.as_bytes(), b" "].concat();
        assert!(
            binary
                .windows(wording.len())
                .any(|window| window == wording.as_slice()),
            "the embedded connector no longer says {UNREAD:?}"
        );
    }

    /// The connector's refusal is a format string kept in pieces; the pieces the refusal is
    /// recognised by are searched for in the embedded connector.
    #[test]
    fn the_connectors_ambiguous_chain_wording_is_pinned() {
        let binary = std::fs::read(std::env::current_exe().expect("this binary")).expect("read");
        for wording in [AMBIGUOUS_CHAIN, AMBIGUOUS_CHAIN_LIST] {
            assert!(
                binary
                    .windows(wording.len())
                    .any(|window| window == wording.as_bytes()),
                "the embedded connector no longer says {wording:?}"
            );
        }
    }

    fn ambiguous() -> (u16, String) {
        (
            400,
            format!(
                "this connector and http://x/ilp{AMBIGUOUS_CHAIN_LIST}evm, solana{AMBIGUOUS_CHAIN}"
            ),
        )
    }

    #[test]
    fn an_ambiguous_chain_refusal_names_the_chains_and_gives_the_amount_back() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![ambiguous()]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(error.message.contains("--chain"), "{}", error.message);
        assert!(error.message.contains("evm, solana"), "{}", error.message);
        assert!(
            error.message.contains("name one as `chain`"),
            "{}",
            error.message
        );
        assert!(crate::spending::failed_before_paying(&error));
        assert_eq!(seen.try_iter().count(), 1);
    }

    #[test]
    fn a_deposit_is_counted_though_the_route_after_it_is_refused_before_anything_is_sent() {
        let home = tempfile::tempdir().expect("a directory");
        let opened = r#"{"id":"far","channel":{"id":"c","status":"opened"}}"#.to_owned();
        let (url, _) = connector(vec![(200, opened), (401, "unauthorized".to_owned())]);
        let mut deposits = 0;
        let Err(error) =
            peer_and_route_on(&surface(home.path(), url), &add(), "g.far", &mut deposits)
        else {
            panic!("the route was made");
        };
        assert_eq!(error.code, ErrorCode::RouteFailed);
        assert!(crate::spending::failed_before_paying(&error));
        assert_eq!(deposits, 1, "the peering's deposit landed before the route");
    }

    #[test]
    fn any_other_400_stays_counted() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, _) = connector(vec![(400, "the deposit is below the minimum".to_owned())]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(!crate::spending::failed_before_paying(&error));
    }

    /// What a request to a connector that answers once with a found channel carried.
    fn request_to_connector(add: &PeerAdd) -> String {
        let home = tempfile::tempdir().expect("a directory");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).expect("a request");
            let (_, body) = found();
            let _ = write!(
                stream,
                "HTTP/1.1 200 X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            String::from_utf8_lossy(&buffer[..read]).into_owned()
        });
        peer_add_on(&surface(home.path(), url), add).expect("peered");
        handle.join().expect("the request")
    }

    #[test]
    fn a_chain_is_sent_as_named_and_not_at_all_otherwise() {
        for (chain, expected) in [
            (Some(Chain::Solana), Some("\"chain\":\"solana\"")),
            (Some(Chain::Evm), Some("\"chain\":\"evm\"")),
            (None, None),
        ] {
            let request = request_to_connector(&PeerAdd { chain, ..add() });
            match expected {
                Some(member) => assert!(request.contains(member), "{request}"),
                None => assert!(!request.contains("\"chain\""), "{request}"),
            }
        }
    }

    #[test]
    fn a_peering_whose_self_description_timed_out_is_repeated_and_reported_as_a_first_success() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![unread(), unread(), found()]);
        let peered = peer_add_on(&surface(home.path(), url), &add()).expect("peered");
        assert!(!peered.deposited);
        assert!(peered.report.text.contains("nothing deposited"));
        assert_eq!(seen.try_iter().count(), 3);
    }

    #[test]
    fn a_peering_whose_self_description_always_timed_out_fails_with_the_refusal() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![unread()]);
        let Err(error) = peer_add_on(&surface(home.path(), url), &add()) else {
            panic!("the peering was made");
        };
        assert_eq!(error.code, ErrorCode::PeerFailed);
        assert!(error.message.contains(UNREAD_TIMEOUT), "{}", error.message);
        assert_eq!(seen.try_iter().count(), 1 + super::UNREAD_REPEATS as usize);
    }

    #[test]
    fn a_self_description_unread_for_another_reason_is_attempted_once() {
        let home = tempfile::tempdir().expect("a directory");
        let refused = (502, format!("{UNREAD} http://x/ilp: connection refused"));
        let (url, seen) = connector(vec![refused]);
        assert!(peer_add_on(&surface(home.path(), url), &add()).is_err());
        assert_eq!(seen.try_iter().count(), 1);
    }

    #[test]
    fn a_stale_read_after_a_timeout_is_still_repeated() {
        let home = tempfile::tempdir().expect("a directory");
        let (url, seen) = connector(vec![unread(), stale(), found()]);
        let peered = peer_add_on(&surface(home.path(), url), &add()).expect("peered");
        assert!(peered.deposited);
        assert_eq!(seen.try_iter().count(), 3);
    }

    /// The connector keeps its wording for a replayed signature in a private type, so it is
    /// pinned through the running connector, by
    /// `the_connectors_replay_wording_is_pinned` in `tests/peer.rs`. This keeps that test
    /// on the wording the repeat is keyed on.
    #[test]
    fn the_replay_wording_pinned_is_the_one_the_repeat_is_keyed_on() {
        let pinned = include_str!("../tests/peer.rs");
        assert!(
            pinned.contains(&format!("text.contains({REPLAYED:?})")),
            "tests/peer.rs no longer pins {REPLAYED:?}"
        );
    }

    /// The connector's `send`, run as `dispatch` runs it, with `operator_key` and `seal_to`.
    fn sent(operator_key: &str, seal_to: &str) -> connector_cli::CliError {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let arguments = [
            "toon send",
            "send",
            "--operator",
            "http://127.0.0.1:1",
            "--operator-key",
            operator_key,
            "--to",
            "g.toon.relay",
            "--seal-to",
            seal_to,
            "--amount",
            "1",
        ];
        match runtime.block_on(connector_cli::run(&arguments)) {
            Err(error) => error,
            Ok(_) => panic!("the send did not fail"),
        }
    }

    /// `before_sending` reads the connector's private `SendError` from its `Debug`, so a
    /// pin move that renames a variant fails here instead of counting these failures again.
    #[test]
    fn a_failure_before_the_write_is_told_from_the_connectors_error() {
        let home = tempfile::tempdir().expect("a directory");
        let missing = home.path().join("missing").to_string_lossy().into_owned();
        assert!(before_sending(&sent(&missing, "http://127.0.0.1:1/ilp")));

        let key = home.path().join("operator.key");
        std::fs::write(&key, [7u8; 32]).expect("the key");
        assert!(before_sending(&sent(
            &key.to_string_lossy(),
            "http://127.0.0.1:1/ilp"
        )));

        assert!(before_sending(&connector_cli::CliError::Usage(
            String::new()
        )));
    }

    #[test]
    fn the_wait_for_a_packet_outlasts_its_expiry() {
        assert!(packet_wait() > PACKET_EXPIRY);
    }
}
