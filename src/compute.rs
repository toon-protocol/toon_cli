//! `toon compute`: renting TOON Compute (spec: `toon-protocol/TOON_Network`,
//! `docs/spec/toon-network-v1.md`, §5 and §6; ADR 0015 for the lease files).
//!
//! A lease is paid per Lease Interval and nothing is refunded. Everything that controls it is
//! its root secret, kept in a lease file that is written before the spawn packet leaves. The
//! operator (with a home) and an agent holding only an agent key (ADR 0010) both rent: they
//! differ in how a packet is paid and how the limits are read, and in nothing else.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use crate::cli::{ComputeCommand, ComputeExtendArgs, ComputeSpawnArgs};
use crate::egress::Egress;
use crate::keystore;
use crate::lease_file::{Lease, Leases, State};
use crate::lease_request::{self, Request, Route};
use crate::listing::{self, Resolved};
use crate::node;
use crate::operator::{self, Answer, Surface};
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::spending;

/// How long a Lease Request is good for (spec §7.2, the Request window is longer).
const REQUEST_TTL_S: u64 = 60;

/// What an agent key may still spend, read from its connector: `None` states no limit.
pub struct AgentLimit {
    pub per_write: Option<u64>,
    pub remaining_today: Option<u64>,
}

/// An agent key with no home.
pub struct AgentRenter {
    pub surface: Surface,
    pub leases: Leases,
    /// Read from the connector, for a command that pays.
    pub limit: Option<AgentLimit>,
}

/// Who rents: the operator, or an agent key with no home.
pub enum Renter<'a> {
    Home(&'a Path),
    Agent(Box<AgentRenter>),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn unsent(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code,
        message,
    }
}

fn usage(message: impl Into<String>) -> Error {
    unsent(ErrorCode::Usage, message.into())
}

fn failure(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code,
        message,
    }
}

// ---- the image and the rest of a spawn's content ----

fn is_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn not_pinned(image: &str, why: &str) -> Error {
    unsent(
        ErrorCode::ImageNotPinned,
        format!(
            "--image {image} {why}. Pin it by digest: `--image <repository>@sha256:<64 hex>`, \
             or `--image sha256:<64 hex>`, with `--registry-entry` if the digest is listed in \
             an Image Registry entry. Nothing was sent; `toon` does not resolve a tag to a \
             digest."
        ),
    )
}

/// The `image` object of a spawn (spec §6.2): exactly one of three forms, always by digest.
pub fn image_object(
    image: &str,
    registry_entry: Option<&str>,
    registry_relay: Option<&str>,
) -> Result<Value, Error> {
    let (reference, digest) = match image.split_once('@') {
        Some((reference, digest)) => (Some(reference), digest),
        None => (None, image),
    };
    if !is_digest(digest) {
        return Err(if digest.starts_with("sha256:") {
            not_pinned(
                image,
                "has a digest that is not `sha256:` and 64 lowercase hex characters",
            )
        } else {
            not_pinned(image, "names a tag, or no digest")
        });
    }
    match (reference, registry_entry) {
        (Some(_), Some(_)) => Err(usage(
            "--registry-entry goes with a digest alone: an image that names its repository \
             is pulled from there, and the two forms are never combined.",
        )),
        (Some(reference), None) => {
            let last = reference.rsplit('/').next().unwrap_or_default();
            if reference.is_empty() || last.contains(':') {
                return Err(not_pinned(
                    image,
                    "names a tag next to its digest, or no repository; the repository is given \
                     without a tag",
                ));
            }
            Ok(json!({ "reference": reference, "digest": digest }))
        }
        (None, None) => Ok(json!({ "digest": digest })),
        (None, Some(address)) => {
            let mut parts = address.splitn(3, ':');
            let valid = matches!(
                (parts.next(), parts.next(), parts.next()),
                (Some("30434"), Some(pubkey), Some(rest))
                    if pubkey.len() == 64
                        && pubkey.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        && rest.split_once(':').is_some_and(|(name, tag)| !name.is_empty() && !tag.is_empty())
            );
            if !valid {
                return Err(usage(format!(
                    "--registry-entry {address} is not an Image Registry entry: it is \
                     `30434:<pubkey>:<name>:<tag>`."
                )));
            }
            let mut entry = json!({ "address": address });
            if let Some(relay) = registry_relay {
                entry["relay"] = json!(relay);
            }
            Ok(json!({ "digest": digest, "registry_entry": entry }))
        }
    }
}

fn ports(texts: &[String]) -> Result<Vec<Value>, Error> {
    texts
        .iter()
        .map(|text| {
            let (number, protocol) = text.split_once('/').unwrap_or((text, "tcp"));
            match (number.parse::<u16>(), protocol) {
                (Ok(port), "tcp" | "udp") if port > 0 => {
                    Ok(json!({ "container_port": port, "protocol": protocol }))
                }
                _ => Err(usage(format!(
                    "--port {text} is not a port: give `<n>`, `<n>/tcp` or `<n>/udp`."
                ))),
            }
        })
        .collect()
}

fn env(texts: &[String]) -> Result<Map<String, Value>, Error> {
    let mut variables = Map::new();
    for text in texts {
        match text.split_once('=') {
            Some((name, value)) if !name.is_empty() => {
                variables.insert(name.to_owned(), json!(value));
            }
            _ => return Err(usage(format!("--env {text} is not `NAME=value`."))),
        }
    }
    Ok(variables)
}

/// The public key `--ssh-key` gives: the key itself, or the file that holds it.
fn ssh_key(text: &str) -> Result<String, Error> {
    let is_key = |text: &str| {
        ["ssh-", "ecdsa-", "sk-"]
            .iter()
            .any(|prefix| text.starts_with(prefix))
    };
    let key = if is_key(text.trim()) {
        text.trim().to_owned()
    } else {
        std::fs::read_to_string(text)
            .map_err(|source| {
                usage(format!(
                    "--ssh-key {text} is neither an SSH public key nor a file that can be read: \
                     {source}."
                ))
            })?
            .trim()
            .to_owned()
    };
    if !is_key(&key) || key.contains('\n') {
        return Err(usage(
            "--ssh-key is not one SSH public key (a line beginning `ssh-ed25519`, `ssh-rsa` and \
             the like).",
        ));
    }
    Ok(key)
}

// ---- packets ----

/// What a provider's answer to a request came to.
enum Reply {
    Body(Value),
    /// The provider's error body (spec §5): it names its `error` and `message`.
    Error {
        error: String,
        message: String,
    },
    /// The connector refused the packet before the provider saw it.
    Rejected {
        code: String,
        message: String,
    },
    Unanswered,
    /// An answer that is not what the spec says a provider answers.
    Strange(String),
}

fn reply_of(answer: Answer) -> Reply {
    match answer {
        Answer::Unanswered | Answer::WrongFulfilment => Reply::Unanswered,
        Answer::Rejected { code, message, .. } => Reply::Rejected { code, message },
        Answer::Fulfilled { body, .. } => match serde_json::from_str::<Value>(&body) {
            Ok(value) if value.is_object() => match value["error"].as_str() {
                Some(error) => Reply::Error {
                    error: error.to_owned(),
                    message: value["message"].as_str().unwrap_or_default().to_owned(),
                },
                None => Reply::Body(value),
            },
            _ => Reply::Strange(format!("the provider answered with {body:?}")),
        },
    }
}

impl Renter<'_> {
    fn leases(&self) -> Leases {
        match self {
            Renter::Home(home) => Leases::of_home(home),
            Renter::Agent(agent) => agent.leases.clone(),
        }
    }

    fn egress(&self) -> Result<Egress, Error> {
        match self {
            Renter::Home(home) => Egress::of(home),
            Renter::Agent(_) => Ok(Egress::direct()),
        }
    }

    /// One packet of `amount` to `destination`, sealed to `seal_key`, with `body` checked
    /// against the shape `route` reads before it leaves.
    fn send(
        &self,
        route: Route,
        destination: &str,
        amount: u64,
        seal_key: &[u8; 65],
        body: &Value,
    ) -> Result<Answer, Error> {
        lease_request::check_shape(route, body)?;
        let bytes = body.to_string().into_bytes();
        match self {
            Renter::Home(home) => operator::dispatch_with_headers(
                home,
                destination,
                amount,
                seal_key,
                ("POST", "/"),
                vec![("content-type".to_owned(), "application/json".to_owned())],
                bytes,
            ),
            Renter::Agent(agent) => operator::write_sealed_as_agent(
                &agent.surface,
                destination,
                amount,
                seal_key,
                bytes,
            ),
        }
    }

    /// Whether a packet to `ilp_address` leaves this agent node: a peering and a route are
    /// there. An agent key has no home to look in, and its connector answers.
    fn reaches(&self, ilp_address: &str, connector_url: &str) -> Result<(), Error> {
        let Renter::Home(home) = self else {
            return Ok(());
        };
        if node::State::load(home)?.is_none() {
            return Err(node::no_agent_node(home));
        }
        if operator::forwards(home, ilp_address)? {
            return Ok(());
        }
        Err(operator::peering_needed(
            home,
            ilp_address,
            connector_url,
            format!(
                "No peering of this agent node reaches {ilp_address}, where the provider is \
                 paid. A peering is needed: run `{}` and then `toon route add {ilp_address} \
                 --peer <id>`.",
                operator::peer_add_command(connector_url, "<amount>", ""),
            ),
        ))
    }
}

fn fresh_hex() -> Result<String, Error> {
    Ok(hex::encode(keystore::random::<32>()?))
}

/// A free request of `op` for `lease`, authorised by its token.
fn lease_request(lease: &Lease, op: &str, content: Value) -> Result<Value, Error> {
    let token = lease_request::continuation(&lease.root()?, &lease.provider);
    Ok(Request {
        op,
        request_id: &fresh_hex()?,
        provider: &lease.provider,
        expiration: now() + REQUEST_TTL_S,
        continuation: &token,
        content,
    }
    .body())
}

fn amount_text(amount: u64) -> String {
    format!("{amount} base units")
}

// ---- spawn ----

/// What `spawn` works out before it pays anything.
struct Plan {
    resolved: Resolved,
    coordinate: listing::Coordinate,
    content: Value,
    /// How many Lease Intervals this command pays: the spawn and `intervals - 1` extends.
    intervals: u64,
    total: u64,
}

/// `--for`'s `<n>s`, `<n>m`, `<n>h`, `<n>d` or `<n>w`, in seconds.
fn duration_s(text: &str) -> Result<u64, Error> {
    let bad = || {
        usage(format!(
            "--for {text} is not a duration: give `<n>s`, `<n>m`, `<n>h`, `<n>d` or `<n>w`, \
             a whole number and a unit."
        ))
    };
    let unit = text.chars().last().ok_or_else(bad)?;
    let each: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return Err(bad()),
    };
    let number: u64 = text[..text.len() - 1].parse().map_err(|_| bad())?;
    number
        .checked_mul(each)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(bad)
}

/// What `price` for `intervals` comes to, refused if it does not fit.
fn total_of(price: u64, intervals: u64) -> Result<u64, Error> {
    price.checked_mul(intervals).ok_or_else(|| {
        usage(format!(
            "{intervals} Lease Intervals at {} each is more than any amount can be.",
            amount_text(price)
        ))
    })
}

/// Refuse, before any packet, a command that pays `total` in packets of `price` each when
/// that breaks the lease limit (counting the `already_paid` of the lease), `toon limit`, or
/// the agent key's limit.
fn limits_refuse(
    renter: &Renter,
    what: &str,
    lease_limit: Option<u64>,
    already_paid: u64,
    price: u64,
    total: u64,
) -> Result<(), Error> {
    let over = |what_limit: &str, limit: u64| {
        unsent(
            ErrorCode::LeaseLimit,
            format!(
                "The {what} would pay {}, which is over {what_limit} of {}. Nothing was sent.",
                amount_text(total),
                amount_text(limit)
            ),
        )
    };
    if let Some(max) = lease_limit.filter(|max| already_paid.saturating_add(total) > *max) {
        let counted = if already_paid > 0 {
            format!(
                "the lease limit (--max), counting the {} the lease has paid already,",
                amount_text(already_paid)
            )
        } else {
            "the lease limit (--max)".to_owned()
        };
        return Err(over(&counted, max));
    }
    if let Renter::Home(home) = renter {
        return spending::check(home, u128::from(total)).map_err(|error| Error {
            nothing_sent: true,
            message: format!(
                "The {what} would pay {}: {} Nothing was sent.",
                amount_text(total),
                error.message
            ),
            ..error
        });
    }
    if let Renter::Agent(agent) = renter {
        let Some(limit) = &agent.limit else {
            return Ok(());
        };
        if let Some(per_write) = limit.per_write.filter(|limit| price > *limit) {
            return Err(Error {
                code: ErrorCode::SpendingLimit,
                ..over(
                    &format!(
                        "the agent key's per-write limit (one packet pays {})",
                        amount_text(price)
                    ),
                    per_write,
                )
            });
        }
        if let Some(remaining) = limit.remaining_today.filter(|left| total > *left) {
            return Err(Error {
                code: ErrorCode::SpendingLimit,
                ..over("what is left of the agent key's per-day limit", remaining)
            });
        }
    }
    Ok(())
}

/// Pay `total` for a command that sends packets: through `toon limit` as one `spend_packets`
/// call with a home, directly with an agent key. `act` is given what moved so far, capped.
fn paying(
    renter: &Renter,
    total: u64,
    act: impl FnOnce(&dyn Fn(u128) -> u128) -> Result<Report, Error>,
) -> Result<Report, Error> {
    match renter {
        Renter::Home(home) => spending::spend_packets(home, u128::from(total), true, |packets| {
            let result = act(&|cap| packets.moved(cap));
            let paid = match &result {
                Ok(_) => u128::from(total),
                Err(error) => error
                    .unanswered
                    .as_ref()
                    .map_or_else(|| packets.moved(u128::from(total)), |u| u.paid),
            };
            result.map(|report| (report, paid))
        }),
        Renter::Agent(_) => act(&|_| 0),
    }
}

fn relays_for(
    renter: &Renter,
    args: &ComputeSpawnArgs,
    coordinate: &listing::Coordinate,
) -> Result<Vec<String>, Error> {
    let mut relays: Vec<String> = args.relays.clone();
    relays.extend(coordinate.relays.iter().cloned());
    if let Renter::Home(home) = renter {
        relays.extend(node::State::load(home)?.and_then(|state| state.relay_url));
    }
    let mut seen = Vec::new();
    relays.retain(|relay| {
        let new = !seen.contains(relay);
        seen.push(relay.clone());
        new
    });
    if relays.is_empty() {
        return Err(usage(
            "There is no relay to read the Listing from: the naddr names none and the network \
             profile follows none. Name one with --relay.",
        ));
    }
    Ok(relays)
}

fn availability_refusal(error: &str, message: &str) -> Error {
    unsent(
        ErrorCode::WouldNotRun,
        format!(
            "The provider says the spawn would not run: {error}: {message}. Nothing was paid; \
             `--no-check` skips this question."
        ),
    )
}

/// The free `availability` route: refuses, for free, a spawn that would not run.
fn check_availability(renter: &Renter, resolved: &Resolved, image: &Value) -> Result<(), Error> {
    let profile = &resolved.profile;
    let body = json!({
        "listing": resolved.listing.name,
        "version": resolved.listing.version,
        "image": image,
    });
    let destination = lease_request::destination(&profile.ilp_address, Route::Availability, "", 0);
    let answer = renter.send(
        Route::Availability,
        &destination,
        0,
        &profile.connector_seal_key,
        &body,
    )?;
    match reply_of(answer) {
        Reply::Body(body) if body["would_run"] == true => Ok(()),
        Reply::Body(body) => Err(availability_refusal(
            body["error"].as_str().unwrap_or("would_run is false"),
            body["message"].as_str().unwrap_or_default(),
        )),
        Reply::Error { error, message } => Err(availability_refusal(&error, &message)),
        Reply::Rejected { code, message } => Err(unsent(
            ErrorCode::SendFailed,
            format!("The availability route was refused with {code}: {message}"),
        )),
        Reply::Unanswered => Err(unsent(
            ErrorCode::SendFailed,
            operator::unanswered_message(),
        )),
        Reply::Strange(what) => Err(unsent(ErrorCode::SendFailed, what)),
    }
}

fn plan(renter: &Renter, args: &ComputeSpawnArgs) -> Result<Plan, Error> {
    let image = image_object(
        &args.image,
        args.registry_entry.as_deref(),
        args.registry_relay.as_deref(),
    )?;
    let mut content = json!({
        "image": image,
        "env": env(&args.env)?,
        "ports": ports(&args.ports)?,
        "ssh_public_key": ssh_key(&args.ssh_key)?,
    });
    if let Some(gb) = args.volume_gb {
        content["volume_gb"] = json!(gb);
    }
    let coordinate = listing::parse_coordinate(&args.listing)?;
    let relays = relays_for(renter, args, &coordinate)?;
    let resolved = listing::resolve(&renter.egress()?, &relays, &coordinate)?;
    // The Profile pins the key a packet is sealed to; the connector must say the same.
    let served = operator::identity(&renter.egress()?, &resolved.profile.connector_url)?;
    if served != resolved.profile.connector_seal_key {
        return Err(unsent(
            ErrorCode::SealKeyMismatch,
            format!(
                "The connector at {} reports a sealing key other than the `connector_seal_key` \
                 of the Profile of {}, so a packet sealed to either would not be the \
                 provider's. Nothing was sent.",
                resolved.profile.connector_url, coordinate.author
            ),
        ));
    }
    let price = resolved.listing.price;
    let intervals = match (&args.for_duration, args.intervals) {
        (Some(text), _) => {
            let seconds = duration_s(text)?;
            let interval = resolved.listing.lease_interval_s.ok_or_else(|| {
                usage(format!(
                    "--for needs the Listing's `lease_interval_s`, and {} states none. Give \
                     --intervals instead.",
                    coordinate.text()
                ))
            })?;
            seconds.div_ceil(interval)
        }
        (None, intervals) => intervals.unwrap_or(1),
    };
    let total = total_of(price, intervals)?;
    Ok(Plan {
        resolved,
        coordinate,
        content,
        intervals,
        total,
    })
}

pub fn spawn(renter: &Renter, args: &ComputeSpawnArgs) -> Result<Report, Error> {
    let Plan {
        resolved,
        coordinate,
        mut content,
        intervals,
        total,
    } = plan(renter, args)?;
    let price = resolved.listing.price;
    let profile = &resolved.profile;
    renter.reaches(&profile.ilp_address, &profile.connector_url)?;
    // Every limit before any packet, the free `availability` question included.
    limits_refuse(renter, "spawn", args.max, 0, price, total)?;
    if let Some(duration) = &args.for_duration {
        eprintln!(
            "--for {duration} rounds up to {}: {} in all.",
            intervals_text(intervals),
            amount_text(total)
        );
    }
    if !args.no_check {
        check_availability(renter, &resolved, &content["image"])?;
    }
    if !args.yes {
        return Err(Error {
            code: ErrorCode::NotConfirmed,
            ..unsent(
                ErrorCode::NotConfirmed,
                format!(
                    "This spawn pays {} for {} of {}, which is not refunded. Add `--yes` to say \
                     that you mean it.",
                    amount_text(total),
                    intervals_text(intervals),
                    coordinate.text()
                ),
            )
        });
    }

    let root = Zeroizing::new(keystore::random::<32>()?);
    let workload_id = fresh_hex()?;
    content["workload_id"] = json!(workload_id);
    let provider = coordinate.author.clone();
    let lease = Lease {
        workload_id: workload_id.clone(),
        root_secret: Zeroizing::new(hex::encode(*root)),
        provider: provider.clone(),
        ilp_address: profile.ilp_address.clone(),
        connector_url: profile.connector_url.clone(),
        connector_seal_key: format!("0x{}", hex::encode(profile.connector_seal_key)),
        listing: coordinate.text(),
        listing_name: resolved.listing.name.clone(),
        listing_version: resolved.listing.version,
        price: Some(price),
        role: "standalone".into(),
        expires_at: None,
        paid: 0,
        lease_limit: args.max,
        state: State::Unknown,
        error: None,
        created_at: now(),
    };
    let request = Request {
        op: "spawn",
        request_id: &fresh_hex()?,
        provider: &provider,
        expiration: now() + REQUEST_TTL_S,
        continuation: &lease_request::continuation(&root, &provider),
        content,
    }
    .body();
    let destination = lease_request::destination(
        &profile.ilp_address,
        Route::Spawn,
        &resolved.listing.name,
        resolved.listing.version,
    );
    let leases = renter.leases();
    paying(renter, total, |moved| -> Result<Report, Error> {
        // The file first: a spawn whose file cannot be written is not sent.
        leases.write(&lease)?;
        let answer = renter
            .send(
                Route::Spawn,
                &destination,
                total,
                &profile.connector_seal_key,
                &request,
            )
            .inspect_err(|error| {
                if error.nothing_sent {
                    // Nothing left, so nothing was paid: the file says the spawn failed.
                    let mut failed = lease.clone();
                    failed.state = State::Failed;
                    failed.error = Some((error.code.as_str().to_owned(), error.message.clone()));
                    let _ = leases.write(&failed);
                }
            })?;
        let (report, mut lease) = settle_spawn(&leases, lease.clone(), answer, price, moved)?;
        if intervals == 1 {
            return Ok(report);
        }
        // The lease is live and paid for: what reaches it is printed whatever the extends do.
        let access = access_text(&report.json["access"]);
        extend_lease(
            renter,
            &leases,
            &mut lease,
            Pay {
                intervals: intervals - 1,
                total,
                bought: 1,
                billed: price,
            },
            moved,
        )
        .map_err(|error| Error {
            message: format!("{}{access}", error.message),
            ..error
        })?;
        Ok(spawn_report(report, &lease, intervals, total))
    })
}

fn intervals_text(intervals: u64) -> String {
    match intervals {
        1 => "one Lease Interval".to_owned(),
        n => format!("{n} Lease Intervals"),
    }
}

/// The spawn's report once the extends that followed it are paid.
fn spawn_report(mut report: Report, lease: &Lease, intervals: u64, total: u64) -> Report {
    let access = report.json["access"].clone();
    report.json["expires_at"] = json!(lease.expires_at);
    report.json["paid"] = json!(lease.paid);
    report.json["intervals"] = json!(intervals);
    report.text = format!(
        "Lease {} is live until {} (unix time), paid {} for {}.{}",
        lease.workload_id,
        lease.expires_at.unwrap_or_default(),
        amount_text(total),
        intervals_text(intervals),
        access_text(&access)
    );
    report
}

/// Record what the spawn's answer says in the lease file and report it, with the lease as
/// settled.
fn settle_spawn(
    leases: &Leases,
    mut lease: Lease,
    answer: Answer,
    total: u64,
    moved: &dyn Fn(u128) -> u128,
) -> Result<(Report, Lease), Error> {
    let id = lease.workload_id.clone();
    let kept = |lease: &Lease| {
        leases.write(lease).err().map(|error| {
            format!(
                " The lease file could not be updated ({}): `toon compute status {id}` settles it.",
                error.message
            )
        })
    };
    match reply_of(answer) {
        Reply::Body(body) => {
            let expires_at = body["expires_at"].as_u64();
            lease.paid = total;
            let Some(expires_at) = expires_at.filter(|_| body["workload_id"] == id.as_str()) else {
                lease.state = State::Unknown;
                kept(&lease);
                return Err(failure(
                    ErrorCode::LeaseRefused,
                    format!(
                        "The provider answered the spawn with something that is not a lease: \
                         {body}. The lease file {id} is marked unknown; `toon compute status \
                         {id}` settles it."
                    ),
                ));
            };
            lease.state = State::Live;
            lease.expires_at = Some(expires_at);
            if let Some(role) = body["role"].as_str() {
                lease.role = role.to_owned();
            }
            let warning = kept(&lease).unwrap_or_default();
            let access = body["access"].clone();
            let report = Report {
                exit: Exit::Success,
                json: json!({
                    "workload_id": id,
                    "provider": lease.provider,
                    "listing": lease.listing,
                    "listing_version": lease.listing_version,
                    "role": lease.role,
                    "expires_at": expires_at,
                    "access": access,
                    "paid": total,
                    "lease_limit": lease.lease_limit,
                }),
                text: format!(
                    "Lease {id} is live until {expires_at} (unix time), paid {}.{}{warning}",
                    amount_text(total),
                    access_text(&access)
                ),
            };
            Ok((report, lease))
        }
        Reply::Error { error, message } => {
            lease.paid = total;
            lease.state = State::Failed;
            lease.error = Some((error.clone(), message.clone()));
            let warning = kept(&lease).unwrap_or_default();
            Err(failure(
                ErrorCode::LeaseRefused,
                format!(
                    "The provider refused the spawn with {error}: {message} It was billed all \
                     the same: {} paid. The lease file {id} is marked failed.{warning}",
                    amount_text(total)
                ),
            ))
        }
        Reply::Rejected { code, message } => {
            lease.paid = u64::try_from(moved(u128::from(total))).unwrap_or(total);
            lease.state = State::Failed;
            lease.error = Some((format!("rejected {code}"), message.clone()));
            let warning = kept(&lease).unwrap_or_default();
            Err(failure(
                ErrorCode::SendFailed,
                format!(
                    "The packet was rejected with {code}: {message} {} paid. The lease file {id} \
                     is marked failed.{warning}",
                    amount_text(lease.paid)
                ),
            ))
        }
        Reply::Unanswered | Reply::Strange(_) => {
            let paid = moved(u128::from(total));
            lease.paid = u64::try_from(paid).unwrap_or(total);
            lease.state = State::Unknown;
            kept(&lease);
            let mut error = operator::unanswered_cost(paid, None);
            error.message = format!(
                "{} The lease file {id} is marked unknown: `toon compute status {id}` asks the \
                 provider whether the spawn took.",
                error.message
            );
            Err(error)
        }
    }
}

fn access_text(access: &Value) -> String {
    let Some(host) = access["host"].as_str() else {
        return String::new();
    };
    let mut text = format!("\nAccess: ssh -p {} {host}", access["ssh_port"]);
    for port in access["ports"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "\n  container port {} is at {host}:{}",
            port["container_port"], port["host_port"]
        ));
    }
    text
}

// ---- extend ----

/// What a run of `.extend` packets is to pay, and what the command has paid before it.
struct Pay {
    /// How many `.extend` packets to send.
    intervals: u64,
    /// What the whole command pays, which caps what `moved` may say.
    total: u64,
    /// The Lease Intervals this command bought before these packets: the spawn's.
    bought: u64,
    /// What this command paid before these packets.
    billed: u64,
}

/// Pay `pay.intervals` `.extend` packets for `lease`, one at a time, recording each answer's
/// real `expires_at` and amount in its file. It stops at the first packet that fails and
/// never retries a paid route by itself.
fn extend_lease(
    renter: &Renter,
    leases: &Leases,
    lease: &mut Lease,
    pay: Pay,
    moved: &dyn Fn(u128) -> u128,
) -> Result<Report, Error> {
    let id = lease.workload_id.clone();
    let price = lease_price(lease)?;
    let paid_start = lease.paid.saturating_sub(pay.billed);
    let destination = lease_request::destination(
        &lease.ilp_address,
        Route::Extend,
        &lease.listing_name,
        lease.listing_version,
    );
    let seal_key = lease.seal_key()?;
    let (mut billed, mut bought) = (pay.billed, pay.bought);
    let wanted = pay.bought + pay.intervals;
    let cause = 'packets: {
        for _ in 0..pay.intervals {
            let answer = match renter.send(
                Route::Extend,
                &destination,
                price,
                &seal_key,
                &json!({ "workload_id": id }),
            ) {
                Ok(answer) => answer,
                Err(error) => break 'packets error,
            };
            let reply = reply_of(answer);
            let mut cause = None;
            match &reply {
                Reply::Body(_) | Reply::Error { .. } => billed += price,
                _ => {
                    billed =
                        billed.max(u64::try_from(moved(u128::from(pay.total))).unwrap_or(pay.total))
                }
            }
            lease.paid = paid_start + billed;
            match reply {
                Reply::Body(body) => {
                    match body["expires_at"]
                        .as_u64()
                        .filter(|_| body["workload_id"] == id.as_str())
                    {
                        Some(expires_at) => {
                            bought += 1;
                            lease.expires_at = Some(expires_at);
                        }
                        None => {
                            cause = Some(failure(
                                ErrorCode::LeaseRefused,
                                format!(
                                    "The provider answered the extend with something that is \
                                     not an extension: {body}. It was billed all the same."
                                ),
                            ));
                        }
                    }
                }
                Reply::Error { error, message } => {
                    if error == "expired" {
                        lease.state = State::Ended;
                    }
                    cause = Some(failure(
                        ErrorCode::LeaseRefused,
                        format!(
                            "The provider refused the extend with {error}: {message} It was \
                             billed all the same."
                        ),
                    ));
                }
                Reply::Rejected { code, message } => {
                    cause = Some(failure(
                        ErrorCode::SendFailed,
                        format!("The extend was rejected with {code}: {message}"),
                    ));
                }
                Reply::Unanswered | Reply::Strange(_) => {
                    cause = Some(operator::unanswered_cost(
                        u128::from(lease.paid - paid_start),
                        None,
                    ));
                }
            }
            // What was bought is in the file before anything else happens.
            if let Err(error) = leases.write(lease) {
                break 'packets cause.unwrap_or(error);
            }
            if let Some(cause) = cause {
                break 'packets cause;
            }
        }
        let now_paid = billed - pay.billed;
        let expires_at = lease.expires_at;
        return Ok(Report {
            exit: Exit::Success,
            json: json!({
                "workload_id": id,
                "intervals": pay.intervals,
                "expires_at": expires_at,
                "paid_now": now_paid,
                "paid": lease.paid,
                "lease_limit": lease.lease_limit,
            }),
            text: format!(
                "Lease {id} is extended by {}, paid {}: it is live until {} (unix time), and \
                 has paid {} in all.",
                intervals_text(pay.intervals),
                amount_text(now_paid),
                expires_at.unwrap_or_default(),
                amount_text(lease.paid)
            ),
        });
    };
    if bought == 0 {
        return Err(cause);
    }
    Err(Error {
        unanswered: cause.unanswered,
        ..failure(
            ErrorCode::LeasePartlyPaid,
            format!(
                "Bought {bought} of {wanted} Lease Intervals and stopped at the first packet that \
             failed: {} Lease {id} runs until {} (unix time) and has paid {} in all, as its \
             file says. Nothing was retried: the routes already paid are not paid again.",
                cause.message,
                lease.expires_at.unwrap_or_default(),
                amount_text(lease.paid)
            ),
        )
    })
}

/// What one `.extend` packet of `lease` pays, as its file states.
fn lease_price(lease: &Lease) -> Result<u64, Error> {
    lease.price.ok_or_else(|| {
        usage(format!(
            "The lease file {} states no price for a Lease Interval, so what an extend pays is \
             not known. Nothing was sent.",
            lease.workload_id
        ))
    })
}

pub fn extend(renter: &Renter, args: &ComputeExtendArgs) -> Result<Report, Error> {
    let leases = renter.leases();
    let mut lease = leases.read(&args.workload)?;
    if lease.state != State::Live {
        return Err(usage(format!(
            "Lease {} is {}, and only a live lease is extended.{}",
            args.workload,
            lease.state.name(),
            if lease.state == State::Unknown {
                format!(" `toon compute status {}` settles it.", args.workload)
            } else {
                String::new()
            }
        )));
    }
    lease.lease_limit = args.max.or(lease.lease_limit);
    let price = lease_price(&lease)?;
    let total = total_of(price, args.intervals)?;
    renter.reaches(&lease.ilp_address, &lease.connector_url)?;
    limits_refuse(
        renter,
        "extend",
        lease.lease_limit,
        lease.paid,
        price,
        total,
    )?;
    if !args.yes {
        return Err(unsent(
            ErrorCode::NotConfirmed,
            format!(
                "This extend pays {} for {} of lease {}, which is not refunded. Add `--yes` to \
                 say that you mean it.",
                amount_text(total),
                intervals_text(args.intervals),
                args.workload
            ),
        ));
    }
    // A changed lease limit is kept before anything is paid.
    leases.write(&lease)?;
    paying(renter, total, |moved| {
        extend_lease(
            renter,
            &leases,
            &mut lease,
            Pay {
                intervals: args.intervals,
                total,
                bought: 0,
                billed: 0,
            },
            moved,
        )
    })
}

// ---- status, terminate, list, forget ----

/// What a lease's provider says about it.
struct Asked {
    report: Report,
}

fn ask(renter: &Renter, lease: &mut Lease, route: Route, op: &str) -> Result<Reply, Error> {
    renter.reaches(&lease.ilp_address, &lease.connector_url)?;
    let request = lease_request(lease, op, json!({ "workload_id": lease.workload_id }))?;
    let destination = lease_request::destination(
        &lease.ilp_address,
        route,
        &lease.listing_name,
        lease.listing_version,
    );
    Ok(reply_of(renter.send(
        route,
        &destination,
        0,
        &lease.seal_key()?,
        &request,
    )?))
}

fn unreached(reply: Reply) -> Error {
    match reply {
        Reply::Rejected { code, message } => failure(
            ErrorCode::SendFailed,
            format!("The packet was rejected with {code}: {message}"),
        ),
        Reply::Unanswered => failure(ErrorCode::SendFailed, operator::unanswered_message()),
        Reply::Strange(what) => failure(ErrorCode::SendFailed, what),
        Reply::Body(_) | Reply::Error { .. } => unreachable!("an answer of the provider"),
    }
}

fn ended(state: &Value) -> bool {
    state.get("ended").is_some()
}

fn status_of(renter: &Renter, leases: &Leases, workload: &str) -> Result<Asked, Error> {
    let mut lease = leases.read(workload)?;
    let was = lease.state;
    match ask(renter, &mut lease, Route::Status, "status")? {
        Reply::Body(body) => {
            lease.state = if ended(&body["state"]) {
                State::Ended
            } else {
                State::Live
            };
            if let Some(expires_at) = body["expires_at"].as_u64() {
                lease.expires_at = Some(expires_at);
            }
            if let Some(role) = body["role"].as_str() {
                lease.role = role.to_owned();
            }
            if was == State::Failed {
                // A refused spawn that now answers is a lease the file did not know of.
                lease.error = None;
            }
            leases.write(&lease)?;
            let access = body["access"].clone();
            Ok(Asked {
                report: Report {
                    exit: Exit::Success,
                    json: json!({
                        "workload_id": lease.workload_id,
                        "role": lease.role,
                        "state": body["state"],
                        "expires_at": lease.expires_at,
                        "access": access,
                        "takeover": body["takeover"],
                    }),
                    text: format!(
                        "Lease {} is {}, until {} (unix time).{}",
                        lease.workload_id,
                        state_text(&body["state"]),
                        lease
                            .expires_at
                            .map_or("unknown".to_owned(), |at| at.to_string()),
                        access_text(&access)
                    ),
                },
            })
        }
        Reply::Error { error, message } => {
            match (error.as_str(), was) {
                ("unknown_workload", State::Unknown) => {
                    lease.state = State::Failed;
                    lease.error = Some((error.clone(), message.clone()));
                    leases.write(&lease)?;
                }
                ("expired", _) => {
                    lease.state = State::Ended;
                    leases.write(&lease)?;
                }
                _ => {}
            }
            Err(failure(
                ErrorCode::LeaseRefused,
                format!("The provider answered {error}: {message}"),
            ))
        }
        other => Err(unreached(other)),
    }
}

fn state_text(state: &Value) -> String {
    match state.as_str() {
        Some(word) => word.to_owned(),
        None => format!("ended ({})", state["ended"].as_str().unwrap_or("unknown")),
    }
}

pub fn status(renter: &Renter, workload: &str) -> Result<Report, Error> {
    Ok(status_of(renter, &renter.leases(), workload)?.report)
}

pub fn terminate(renter: &Renter, workload: &str, yes: bool) -> Result<Report, Error> {
    let leases = renter.leases();
    let mut lease = leases.read(workload)?;
    if !yes {
        let left = match lease.expires_at {
            Some(at) if at > now() => {
                let seconds = at - now();
                format!(
                    "the {} minute(s) paid until {at} (unix time)",
                    seconds.div_ceil(60)
                )
            }
            _ => "whatever paid time is left".to_owned(),
        };
        return Err(Error {
            code: ErrorCode::NotConfirmed,
            ..unsent(
                ErrorCode::NotConfirmed,
                format!(
                    "Terminating lease {workload} loses {left}: nothing is refunded. Add \
                     `--yes` to say that you mean it."
                ),
            )
        });
    }
    match ask(renter, &mut lease, Route::Terminate, "terminate")? {
        Reply::Body(body) => {
            lease.state = State::Ended;
            leases.write(&lease)?;
            Ok(Report {
                exit: Exit::Success,
                json: json!({ "workload_id": lease.workload_id, "state": body["state"] }),
                text: format!(
                    "Lease {} is terminated; nothing is refunded.",
                    lease.workload_id
                ),
            })
        }
        Reply::Error { error, message } => {
            if error == "expired" {
                lease.state = State::Ended;
                leases.write(&lease)?;
            }
            Err(failure(
                ErrorCode::LeaseRefused,
                format!("The provider answered {error}: {message}"),
            ))
        }
        other => Err(unreached(other)),
    }
}

fn row(lease: &Lease) -> String {
    let none = || "none".to_owned();
    format!(
        "{} {} {}@v{} {} provider {} expires {} paid {} limit {}{}",
        lease.workload_id,
        lease.state.name(),
        lease.listing_name,
        lease.listing_version,
        lease.role,
        &lease.provider[..lease.provider.len().min(8)],
        lease
            .expires_at
            .map_or_else(|| "-".to_owned(), |at| at.to_string()),
        lease.paid,
        lease
            .lease_limit
            .map_or_else(none, |limit| limit.to_string()),
        lease
            .error
            .as_ref()
            .map(|(code, _)| format!(" error {code}"))
            .unwrap_or_default()
    )
}

pub fn list(renter: &Renter, check: bool) -> Result<Report, Error> {
    let leases = renter.leases();
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    for lease in leases.list()? {
        let mut entry = lease.public();
        let mut line = row(&lease);
        if check {
            match status_of(renter, &leases, &lease.workload_id) {
                Ok(asked) => {
                    entry["check"] = asked.report.json;
                    line.push_str(&format!(" | {}", asked.report.text));
                }
                Err(error) => {
                    entry["check"] = json!({ "error": { "code": error.code.as_str(), "message": error.message } });
                    line.push_str(&format!(" | not checked: {}", error.message));
                }
            }
        }
        entries.push(entry);
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(format!("No leases in {}.", leases.dir().display()));
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "leases": entries }),
        text: lines.join("\n"),
    })
}

pub fn forget(renter: &Renter, workload: &str) -> Result<Report, Error> {
    let leases = renter.leases();
    let lease = leases.read(workload)?;
    let may_be_live = matches!(lease.state, State::Live | State::Unknown)
        && lease.expires_at.is_none_or(|at| at > now());
    leases.remove(workload)?;
    let warning = may_be_live.then(|| {
        format!(
            "The lease may still be live{}: its root secret went with the file, so nobody can \
             read, extend or end it any more.",
            lease
                .expires_at
                .map(|at| format!(" until {at} (unix time)"))
                .unwrap_or_default()
        )
    });
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "workload_id": workload, "forgotten": true, "warning": warning }),
        text: match &warning {
            Some(warning) => format!("Removed the lease file of {workload}. {warning}"),
            None => format!("Removed the lease file of {workload}."),
        },
    })
}

/// `toon compute` for a renter.
pub fn run(renter: &Renter, command: &ComputeCommand) -> Result<Report, Error> {
    match command {
        ComputeCommand::Spawn(args) => spawn(renter, args),
        ComputeCommand::Extend(args) => extend(renter, args),
        ComputeCommand::Status { workload } => status(renter, workload),
        ComputeCommand::Terminate { workload, yes } => terminate(renter, workload, *yes),
        ComputeCommand::List { check } => list(renter, *check),
        ComputeCommand::Forget { workload } => forget(renter, workload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:c02a7c7b9cac8e709f8cc42dc9e7aa8fdc36e35acfa19da7cd7dff2b8bd94fdd";

    #[test]
    fn the_three_image_forms_are_built_and_a_tag_is_not_pinned() {
        assert_eq!(
            image_object(&format!("docker.io/library/alpine@{DIGEST}"), None, None).unwrap(),
            json!({ "reference": "docker.io/library/alpine", "digest": DIGEST })
        );
        assert_eq!(
            image_object(DIGEST, None, None).unwrap(),
            json!({ "digest": DIGEST })
        );
        let entry = format!("30434:{}:web:1.0", "2c".repeat(32));
        assert_eq!(
            image_object(DIGEST, Some(&entry), Some("ws://relay.test:7100")).unwrap(),
            json!({ "digest": DIGEST, "registry_entry": { "address": entry, "relay": "ws://relay.test:7100" } })
        );
        for tagged in [
            "alpine:3.20",
            "alpine",
            "docker.io/library/alpine:latest",
            "sha256:abc",
        ] {
            let error = image_object(tagged, None, None).expect_err(tagged);
            assert_eq!(error.code, ErrorCode::ImageNotPinned, "{tagged}");
        }
        assert!(image_object(&format!("alpine:3@{DIGEST}"), None, None).is_err());
        assert!(image_object(&format!("alpine@{DIGEST}"), Some(&entry), None).is_err());
        assert!(image_object(DIGEST, Some("30434:nope"), None).is_err());
    }

    #[test]
    fn a_duration_is_a_whole_number_and_a_unit() {
        assert_eq!(duration_s("90s").unwrap(), 90);
        assert_eq!(duration_s("150m").unwrap(), 9000);
        assert_eq!(duration_s("2h").unwrap(), 7200);
        assert_eq!(duration_s("1d").unwrap(), 86_400);
        assert_eq!(duration_s("1w").unwrap(), 604_800);
        for bad in ["", "h", "10", "0m", "1.5h", "-1h", "5y"] {
            assert!(duration_s(bad).is_err(), "{bad}");
        }
        assert_eq!(7200_u64.div_ceil(3600), 2);
        assert_eq!(7201_u64.div_ceil(3600), 3);
        assert!(total_of(u64::MAX, 2).is_err());
    }

    #[test]
    fn ports_and_variables_are_parsed() {
        assert_eq!(
            ports(&["443".into(), "53/udp".into()]).unwrap(),
            vec![
                json!({ "container_port": 443, "protocol": "tcp" }),
                json!({ "container_port": 53, "protocol": "udp" })
            ]
        );
        assert!(ports(&["http".into()]).is_err());
        assert!(ports(&["80/sctp".into()]).is_err());
        assert_eq!(env(&["A=b=c".into()]).unwrap()["A"], "b=c");
        assert!(env(&["nope".into()]).is_err());
    }
}
