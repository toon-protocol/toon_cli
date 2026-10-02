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

use crate::cli::{ChannelCommand, JoinArgs, PeerCommand, RouteCommand};
use crate::control;
use crate::egress::Egress;
use crate::node::{self, ConnectorFiles, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};
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
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error {
        code,
        message,
        nothing_sent: false,
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

/// A write to the connector's operator surface, signed with the wallet's operator write
/// key. `body` is JSON, or empty for a delete. Returns the status and the body.
fn write(
    surface: &Surface,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, String), Error> {
    let raw = std::fs::read(&surface.write_key).map_err(|source| {
        failed(
            ErrorCode::Io,
            format!("{}: {source}.", surface.write_key.display()),
        )
    })?;
    let keypair = ed25519_dalek_v1::SecretKey::from_bytes(&raw)
        .map(|secret| ed25519_dalek_v1::Keypair {
            public: (&secret).into(),
            secret,
        })
        .map_err(|_| {
            failed(
                ErrorCode::Io,
                format!("{} is not a 32-byte key.", surface.write_key.display()),
            )
        })?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (signature_input, signature, content_digest) = connector_operator::signing::sign_request(
        &keypair,
        method.as_str(),
        path,
        body.as_bytes(),
        created,
        Some(created + SIGNATURE_TTL),
    );
    let url = format!("{}{path}", surface.url);
    let mut request = reqwest::blocking::Client::builder()
        .timeout(WRITE_PATIENCE)
        .build()
        .map_err(|error| {
            failed(
                ErrorCode::ConnectorFailed,
                format!("{method} {url}: {error}."),
            )
        })?
        .request(method.clone(), &url)
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature);
    if !body.is_empty() {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    let response = request.send().map_err(|error| {
        failed(
            ErrorCode::ConnectorFailed,
            format!("{method} {url}: {error}."),
        )
    })?;
    let status = response.status().as_u16();
    let text = response.text().unwrap_or_default();
    Ok((status, text))
}

/// `toon route list`: the connector's routing table, as it answers it: the routes it
/// terminates, then the routes it forwards.
pub fn route_list(home: &Path) -> Result<Report, Error> {
    let surface = surface(home)?;
    let routes = read(&surface, "/routes")?;
    let forwarding = read(&surface, "/routes/peers")?;
    let mut lines: Vec<String> = routes
        .iter()
        .map(|route| {
            format!(
                "{} -> {} (price {})",
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
/// channel whose watermark the connector does not report counts as 0.
pub fn outbound_watermark(home: &Path) -> Result<u128, Error> {
    Ok(channels_on(&surface(home)?)?
        .iter()
        .filter(|channel| channel["direction"] == "outbound")
        .filter_map(|channel| match &channel["watermark"] {
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

/// What `toon peer add` was asked for.
pub struct PeerAdd<'a> {
    pub address: &'a str,
    pub deposit: u128,
    pub id: Option<&'a str>,
    pub fee: u64,
    pub max_packet_amount: u64,
}

/// `toon peer add`: create a peering toward the connector at `address`, opening and
/// funding the channel it pays on with `deposit`.
pub fn peer_add(home: &Path, add: &PeerAdd) -> Result<Report, Error> {
    peer_add_on(&surface(home)?, add)
}

/// `peer_add`, on the connector whose operator surface is `surface`.
pub fn peer_add_on(surface: &Surface, add: &PeerAdd) -> Result<Report, Error> {
    let id = add.id.map_or_else(|| label(add.address), str::to_owned);
    segment(ErrorCode::PeerFailed, "The peering's label", &id)?;
    let body = json!({
        "id": id,
        "url": add.address,
        "fee": add.fee,
        "max_packet_amount": add.max_packet_amount,
        "deposit": add.deposit,
    });
    let (status, text) = write(surface, reqwest::Method::POST, "/peers", Some(&body))?;
    if status != 200 {
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
            failed(ErrorCode::PeerFailed, refusal(status, &text))
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
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "peering": peering }),
        text: format!(
            "Peered with {} as {id}, paying on channel {channel} ({channel_status}), deposit {}.\n\
             Packets forwarded to it are served by that connector. It forwards back to you \
             only if its operator creates a peering toward you in return.",
            add.address, add.deposit
        ),
    })
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
        return Err(failed(ErrorCode::PeerFailed, refusal(status, &text)));
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
        return Err(failed(ErrorCode::RouteFailed, refusal(status, &text)));
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
        return Err(failed(ErrorCode::RouteFailed, refusal(status, &text)));
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
    let channel_failed = |message: String| failed(ErrorCode::ChannelFailed, message);
    let keypair = write_keypair(&surface.write_key)?;
    let bytes = body.into_bytes();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (signature_input, signature, content_digest) = connector_operator::signing::sign_request(
        &keypair,
        "POST",
        path,
        &bytes,
        created,
        Some(created + SIGNATURE_TTL),
    );
    let url = format!("{}{path}", surface.url);
    let mut request = reqwest::blocking::Client::builder()
        .timeout(PATIENCE)
        .build()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?
        .post(&url)
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature);
    if !bytes.is_empty() {
        request = request.header("content-type", "application/json");
    }
    let response = request
        .body(bytes)
        .send()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| channel_failed(format!("POST {url}: {error}.")))?;
    if !status.is_success() {
        return Err(channel_failed(format!(
            "POST {url} answered {status}: {}",
            text.trim()
        )));
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
    Fulfilled { status: u64, body: String },
    Rejected { code: String, message: String },
    WrongFulfilment,
}

/// Read the connector's one-line summary of a send. The connector's own `send` verb is
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
    Some(Answer::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

/// Whether the connector's `send` failed before it wrote to the operator surface: its
/// arguments were refused, the operator key could not be read, the identity to seal to
/// could not be fetched, or the packet could not be sealed. A `Transport` failure may
/// come after the packet left, and an answer not understood does, so neither is here.
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
        }
        _ => false,
    }
}

/// Send one packet from the operator surface to `destination`, for `amount`, sealed to the
/// connector at `seal_to`, or to this one, and return what the connector says it came to.
/// `body` is a file the request carries as its JSON body.
pub fn dispatch(
    home: &Path,
    destination: &str,
    amount: u64,
    seal_to: Option<&str>,
    body: Option<&Path>,
) -> Result<Answer, Error> {
    let surface = surface(home)?;
    let send_failed = |message: String| failed(ErrorCode::SendFailed, message);
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
        return dispatch_with_headers(home, destination, amount, &public, headers, body);
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
/// is not fetched from anywhere, with `headers` on the request and `body` in memory. The
/// connector's `send` fixes the request's headers, so this forms, seals and
/// signs the packet itself, with the connector's own crates, and reads the answer the
/// same way.
pub fn dispatch_with_headers(
    home: &Path,
    destination: &str,
    amount: u64,
    public: &[u8; 65],
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
        .timeout(PATIENCE)
        .build()
        .map_err(|error| not_sent(error.to_string()))?;

    let plaintext = EnvelopeRequest {
        method: "POST".into(),
        target: "/".into(),
        headers,
        body,
    }
    .encode();
    let (data, secret) = seal_request(&plaintext, public)
        .map_err(|error| not_sent(format!("The packet could not be sealed: {error}.")))?;
    let prepare = Prepare {
        amount,
        expires_at: chrono::Utc::now() + chrono::Duration::seconds(30),
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
    let response = client
        .post(format!("{}/packets", surface.url))
        .header("content-type", "application/octet-stream")
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature)
        .body(prepare)
        .send()
        .map_err(|error| send_failed(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .map_err(|error| send_failed(error.to_string()))?;
    if !status.is_success() {
        return Err(send_failed(format!(
            "The connector refused the write with {status}: {}",
            String::from_utf8_lossy(&bytes).trim()
        )));
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
            Ok(Answer::Rejected {
                code: reject.code.as_str().to_owned(),
                message: reject.message,
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
) -> Result<Report, Error> {
    let answer = dispatch(home, destination, amount, seal_to, None)?;
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
        Answer::Rejected { code, message } => Report {
            exit: Exit::Failure,
            json: json!({
                "destination": destination,
                "amount": amount,
                "outcome": "rejected",
                "reject": { "code": code, "message": message },
            }),
            text: format!("Rejected with {code}: {sent}. {message}"),
        },
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
                let report = peer_add(
                    home,
                    &PeerAdd {
                        address: &args.address,
                        deposit: args.deposit,
                        id: args.id.as_deref(),
                        fee: args.fee,
                        max_packet_amount: args.max_packet_amount,
                    },
                )?;
                Ok((report, true))
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
            },
        )?;
        let routed = route_add(home, NETWORK_PREFIX, name, 0)?;
        let relay = state.relay_url.clone();
        state.joined = Some(name.to_owned());
        if let Some(relay) = &relay {
            if !state.reads.contains(relay) {
                state.reads.push(relay.clone());
            }
        }
        state.save(home)?;
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
                }),
                text: format!(
                    "Joined {name}: peered with {} and forwarding {NETWORK_PREFIX} to it, deposit {}. \
                     {reading}\n\
                     Its connector forwards back to you only if its operator creates a peering toward you in return.",
                    connector_url, args.deposit
                ),
            },
            true,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::before_sending;

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
}
