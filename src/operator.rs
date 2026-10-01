//! The connector's operator surface, used the way a remote operator would use it.
//!
//! Reads carry the connector's bearer token. A write is signed with the wallet's
//! operator write key, whose public half the connector's config lists. Both go over
//! loopback to the address the supervisor says its connector listens on.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::control;
use crate::node::{self, ConnectorFiles, State};
use crate::outcome::{Error, ErrorCode, Exit, Report};

/// How long a read of the operator surface waits for the connector.
const PATIENCE: Duration = Duration::from_secs(30);

/// A running connector's operator surface.
struct Surface {
    /// `http://host:port`, with no trailing slash.
    url: String,
    bearer_token: PathBuf,
    write_key: PathBuf,
    /// Whether the connector may peer toward a plain `http://` address.
    plaintext_peers: bool,
}

fn failed(code: ErrorCode, message: String) -> Error {
    Error { code, message }
}

/// The operator surface of the first TOON app's connector, which must be running.
fn surface(home: &Path) -> Result<Surface, Error> {
    let Some(state) = State::load(home)? else {
        return Err(node::no_agent_node(home));
    };
    let app = &state.toon_apps[0];
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
    let surface = surface(home)?;
    let id = add.id.map_or_else(|| label(add.address), str::to_owned);
    segment(ErrorCode::PeerFailed, "The peering's label", &id)?;
    let body = json!({
        "id": id,
        "url": add.address,
        "fee": add.fee,
        "max_packet_amount": add.max_packet_amount,
        "deposit": add.deposit,
    });
    let (status, text) = write(&surface, reqwest::Method::POST, "/peers", Some(&body))?;
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
    let surface = surface(home)?;
    let body = json!({ "prefix": prefix, "peer_id": peer, "price": price });
    let (status, text) = write(
        &surface,
        reqwest::Method::POST,
        "/routes/peers",
        Some(&body),
    )?;
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

/// What a packet came to.
enum Answer {
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

/// `toon send`: one packet from the operator surface to `destination`, for `amount`,
/// sealed to the connector at `seal_to`, or to this one.
/// A packet that is not fulfilled is a report, not an error, and exits 1.
pub fn send(
    home: &Path,
    destination: &str,
    amount: u64,
    seal_to: Option<&str>,
) -> Result<Report, Error> {
    let surface = surface(home)?;
    let send_failed = |message: String| failed(ErrorCode::SendFailed, message);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| send_failed(format!("The async runtime did not start: {error}.")))?;
    let amount_text = amount.to_string();
    let seal_to = seal_to.map_or_else(|| format!("{}/ilp", surface.url), str::to_owned);
    let write_key = surface.write_key.to_string_lossy();
    let summary = runtime
        .block_on(connector_cli::run(&[
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
        ]))
        .map_err(|error| send_failed(error.to_string()))
        .and_then(|command| match command {
            connector_cli::Command::Finished { summary } => Ok(summary),
            connector_cli::Command::Serve(_) => {
                Err(send_failed("The connector did not send.".into()))
            }
        })?;
    let answer = answer(&summary).ok_or_else(|| {
        send_failed(format!(
            "The connector's answer was not understood: {summary}"
        ))
    })?;
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
