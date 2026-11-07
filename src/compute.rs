//! `toon compute`: renting TOON Compute (spec: `toon-protocol/TOON_Network`,
//! `docs/spec/toon-network-v1.md`, §5 and §6; ADR 0015 for the lease files).
//!
//! A lease is paid per Lease Interval and nothing is refunded. Everything that controls it is
//! its root secret, kept in a lease file that is written before the spawn packet leaves. The
//! operator (with a home) and an agent holding only an agent key (ADR 0010) both rent: they
//! differ in how a packet is paid and how the limits are read, and in nothing else.

use std::cell::Cell;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use crate::cli::{ComputeCommand, ComputeExtendArgs, ComputeSpawnArgs};
use crate::deployment::{self, Tags};
use crate::egress::Egress;
use crate::keystore;
use crate::lease_file::{Claim, Lease, Leases, State};
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

/// Signs and, if told to, publishes a Deployment's tags and content.
pub type SignDeployment = Box<dyn Fn(&Tags, &str, bool) -> Result<(), Error>>;

/// An agent key with no home.
pub struct AgentRenter {
    pub surface: Surface,
    pub leases: Leases,
    /// Read from the connector, for a command that pays.
    pub limit: Option<AgentLimit>,
    /// Signs a Deployment's tags with the agent identity (the `sign` order) and, when the last
    /// argument is true, writes it to the agent node's own relay. False signs and discards: a
    /// probe of the key's scope, free.
    pub sign_deployment: SignDeployment,
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
    let token = lease.token(&lease.provider)?;
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

/// One member of what `spawn` buys: a Listing and its provider's Profile.
struct Target {
    resolved: Resolved,
    coordinate: listing::Coordinate,
    /// What one Lease Interval of this member costs: the Listing's `price` for the primary or a
    /// standalone, its `standby_price` for a standby.
    price: u64,
}

/// What `spawn` works out before it pays anything.
struct Plan {
    /// The primary first, then each standby in the order given.
    targets: Vec<Target>,
    content: Value,
    /// How many Lease Intervals this command pays for each member: the spawn and
    /// `intervals - 1` extends.
    intervals: u64,
    total: u64,
    /// The dearest packet: what a per-write limit is held against.
    max_price: u64,
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
        return Err(usage(match renter {
            Renter::Home(_) => {
                "There is no relay to read the Listing from: the naddr names none and the \
                 network profile follows none. Name one with --relay."
            }
            Renter::Agent(_) => {
                "There is no relay to read the Listing from: an agent key has no home to read a \
                 relay from, and the naddr names none. Name one with --relay or give an naddr \
                 with relay hints."
            }
        }));
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

/// The free `availability` route: refuses, for free, a spawn that would not run. A member of
/// a Standby Set is asked with its `role` (spec §6.4).
fn check_availability(
    renter: &Renter,
    target: &Target,
    image: &Value,
    role: Option<&str>,
) -> Result<(), Error> {
    let (resolved, profile) = (&target.resolved, &target.resolved.profile);
    let mut body = json!({
        "listing": resolved.listing.name,
        "version": resolved.listing.version,
        "image": image,
    });
    if let Some(role) = role {
        body["role"] = json!(role);
    }
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

/// Read the Listing `text` names and its provider's Profile, and check the connector says
/// the seal key the Profile does.
fn resolve_target(
    renter: &Renter,
    args: &ComputeSpawnArgs,
    text: &str,
) -> Result<(Resolved, listing::Coordinate), Error> {
    let coordinate = listing::parse_coordinate(text)?;
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
    Ok((resolved, coordinate))
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
    let mut targets = Vec::new();
    let (resolved, coordinate) = resolve_target(renter, args, &args.listing)?;
    let price = resolved.listing.price;
    targets.push(Target {
        resolved,
        coordinate,
        price,
    });
    for text in &args.standbys {
        let (resolved, coordinate) = resolve_target(renter, args, text)?;
        let Some(price) = resolved.listing.standby_price else {
            return Err(usage(format!(
                "{} states no `standby_price`, so it sells no Warm Standby. Nothing was sent.",
                coordinate.text()
            )));
        };
        if let Some(first) = targets
            .iter()
            .find(|target| target.coordinate.author == coordinate.author)
        {
            return Err(usage(format!(
                "--standby {} is provider {}, which is already in the Standby Set as {}: a \
                 provider is in a set once.",
                coordinate.text(),
                coordinate.author,
                first.coordinate.text()
            )));
        }
        targets.push(Target {
            resolved,
            coordinate,
            price,
        });
    }
    let primary = &targets[0];
    let intervals = match (&args.for_duration, args.intervals) {
        (Some(text), _) => {
            let seconds = duration_s(text)?;
            let interval = primary.resolved.listing.lease_interval_s.ok_or_else(|| {
                usage(format!(
                    "--for needs the Listing's `lease_interval_s`, and {} states none. Give \
                     --intervals instead.",
                    primary.coordinate.text()
                ))
            })?;
            seconds.div_ceil(interval)
        }
        (None, intervals) => intervals.unwrap_or(1),
    };
    let mut total = 0u64;
    for target in &targets {
        total = total
            .checked_add(total_of(target.price, intervals)?)
            .ok_or_else(|| usage("The total is more than any amount can be."))?;
    }
    let max_price = targets.iter().map(|target| target.price).max().unwrap_or(0);
    Ok(Plan {
        targets,
        content,
        intervals,
        total,
        max_price,
    })
}

// ---- what one command has paid and bought ----

/// What the packets of one command have cost and bought so far, across every member of the
/// lease. `moved` is the connector's own count of what left, for a packet nobody answered.
struct Ledger<'a> {
    /// What the command pays in all, which caps what `moved` may say.
    total: u64,
    moved: &'a dyn Fn(u128) -> u128,
    /// How many packets the command means to pay.
    wanted: u64,
    counted: Cell<u64>,
    bought: Cell<u64>,
}

impl<'a> Ledger<'a> {
    fn new(total: u64, moved: &'a dyn Fn(u128) -> u128, wanted: u64) -> Self {
        Self {
            total,
            moved,
            wanted,
            counted: Cell::new(0),
            bought: Cell::new(0),
        }
    }

    /// A packet the provider answered, with a lease or an error: billed in full.
    fn answered(&self, price: u64) -> u64 {
        self.counted.set(self.counted.get().saturating_add(price));
        price
    }

    /// A packet nobody answered, or the connector refused: what moved since the last packet,
    /// at most its price.
    fn unanswered(&self, price: u64) -> u64 {
        let moved = u64::try_from((self.moved)(u128::from(self.total))).unwrap_or(self.total);
        let amount = moved.saturating_sub(self.counted.get()).min(price);
        self.counted.set(self.counted.get() + amount);
        amount
    }

    fn paid(&self) -> u64 {
        self.counted.get()
    }

    /// A packet that bought a Lease Interval.
    fn bought_one(&self) {
        self.bought.set(self.bought.get() + 1);
    }
}

fn short(provider: &str) -> &str {
    &provider[..provider.len().min(8)]
}

fn paid_in_all(members: &[Lease]) -> u64 {
    members.iter().map(|member| member.paid).sum()
}

/// What each member runs until, for a message.
fn members_text(members: &[Lease]) -> String {
    members
        .iter()
        .map(|member| {
            let until = match (member.state, member.expires_at) {
                (State::Live, Some(at)) => format!("runs until {at} (unix time)"),
                (state, _) => state.name().to_owned(),
            };
            format!("{} {} {until}", member.role, short(&member.provider))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A command that paid several packets and failed at one of them after at least one bought
/// time: say what was bought. A command that bought nothing fails as its cause did.
fn partly_paid(cause: Error, members: &[Lease], ledger: &Ledger) -> Error {
    let bought = ledger.bought.get();
    if bought == 0 {
        return cause;
    }
    Error {
        unanswered: cause.unanswered,
        ..failure(
            ErrorCode::LeasePartlyPaid,
            format!(
                "Bought {bought} of {} Lease Intervals and stopped at the first packet that \
                 failed: {} Lease {} has paid {} in all, as its file says: {}. Nothing was \
                 retried: the routes already paid are not paid again.",
                ledger.wanted,
                cause.message,
                members[0].workload_id,
                amount_text(paid_in_all(members)),
                members_text(members)
            ),
        )
    }
}

/// What the lease's file says of one member, for a report.
fn member_json(lease: &Lease) -> Value {
    json!({
        "provider": lease.provider,
        "listing": lease.listing,
        "listing_version": lease.listing_version,
        "role": lease.role,
        "state": lease.state.name(),
        "expires_at": lease.expires_at,
        "paid": lease.paid,
    })
}

fn members_json(members: &[Lease]) -> Value {
    Value::Array(members.iter().map(member_json).collect())
}

/// Write the lease file after a packet's answer, giving back a sentence for the report when it
/// could not be.
fn keep(leases: &Leases, members: &[Lease]) -> String {
    match leases.write(members) {
        Ok(()) => String::new(),
        Err(error) => format!(
            " The lease file could not be updated ({}): `toon compute status {}` settles it.",
            error.message, members[0].workload_id
        ),
    }
}

// ---- spawn ----

pub fn spawn(renter: &Renter, args: &ComputeSpawnArgs) -> Result<Report, Error> {
    let Plan {
        targets,
        content: base,
        intervals,
        total,
        max_price,
    } = plan(renter, args)?;
    for target in &targets {
        let profile = &target.resolved.profile;
        renter.reaches(&profile.ilp_address, &profile.connector_url)?;
    }
    // Every limit before any packet, the free `availability` question included.
    limits_refuse(renter, "spawn", args.max, 0, max_price, total)?;
    if let Some(duration) = &args.for_duration {
        eprintln!(
            "--for {duration} rounds up to {}: {} in all.",
            intervals_text(intervals),
            amount_text(total)
        );
    }
    let in_set = targets.len() > 1;
    let role_of = |index: usize| match (in_set, index) {
        (false, _) => "standalone",
        (true, 0) => "primary",
        (true, _) => "standby",
    };
    let root = Zeroizing::new(keystore::random::<32>()?);
    let workload_id = fresh_hex()?;
    let standby_set: Vec<String> = if in_set {
        targets
            .iter()
            .map(|target| target.coordinate.author.clone())
            .collect()
    } else {
        Vec::new()
    };
    let planned: Vec<Lease> = targets
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let profile = &target.resolved.profile;
            Lease {
                workload_id: workload_id.clone(),
                root_secret: Zeroizing::new(hex::encode(*root)),
                provider: target.coordinate.author.clone(),
                ilp_address: profile.ilp_address.clone(),
                connector_url: profile.connector_url.clone(),
                connector_seal_key: format!("0x{}", hex::encode(profile.connector_seal_key)),
                listing: target.coordinate.text(),
                listing_name: target.resolved.listing.name.clone(),
                listing_version: target.resolved.listing.version,
                price: Some(target.price),
                role: role_of(index).into(),
                expires_at: None,
                paid: 0,
                lease_limit: args.max,
                state: State::Unknown,
                error: None,
                created_at: now(),
                next_secret: None,
                rotated: Vec::new(),
                deployment: claim(args),
            }
        })
        .collect();
    // A Deployment that cannot be made, or a key that cannot sign it, is refused before any
    // packet is sent.
    if let Some(claim) = &planned[0].deployment {
        deploy(renter, &planned, claim, now() + REQUEST_TTL_S, true)?;
    }
    if !args.no_check {
        for (index, target) in targets.iter().enumerate() {
            let role = in_set.then(|| role_of(index));
            check_availability(renter, target, &base["image"], role)?;
        }
    }
    if !args.yes {
        let set = if in_set {
            format!(
                ", and {} for each of {} Warm Standby(s)",
                intervals_text(intervals),
                targets.len() - 1
            )
        } else {
            String::new()
        };
        return Err(Error {
            code: ErrorCode::NotConfirmed,
            ..unsent(
                ErrorCode::NotConfirmed,
                format!(
                    "This spawn pays {} for {} of {}{set}, which is not refunded. Add `--yes` to \
                     say that you mean it.",
                    amount_text(total),
                    intervals_text(intervals),
                    targets[0].coordinate.text()
                ),
            )
        });
    }

    let leases = renter.leases();
    paying(renter, total, |moved| -> Result<Report, Error> {
        let ledger = Ledger::new(total, moved, intervals * targets.len() as u64);
        let mut members: Vec<Lease> = Vec::new();
        let mut access = Value::Null;
        let bought = (|| -> Result<(), Error> {
            for (index, target) in targets.iter().enumerate() {
                let profile = &target.resolved.profile;
                let provider = target.coordinate.author.clone();
                members.push(planned[index].clone());
                let request = Request {
                    op: if index == 0 { "spawn" } else { "standby" },
                    request_id: &fresh_hex()?,
                    provider: &provider,
                    expiration: now() + REQUEST_TTL_S,
                    continuation: &lease_request::continuation(&root, &provider),
                    content: lease_request::member_content(&base, &workload_id, &standby_set),
                }
                .body();
                let (route, listing) = (
                    if index == 0 {
                        Route::Spawn
                    } else {
                        Route::Standby
                    },
                    &target.resolved.listing,
                );
                let destination = lease_request::destination(
                    &profile.ilp_address,
                    route,
                    &listing.name,
                    listing.version,
                );
                // The file first: a packet whose file cannot be written is not sent.
                leases.write(&members).inspect_err(|_| {
                    members.pop();
                })?;
                let answer = renter
                    .send(
                        route,
                        &destination,
                        target.price,
                        &profile.connector_seal_key,
                        &request,
                    )
                    .inspect_err(|error| {
                        if !error.nothing_sent {
                            return;
                        }
                        // Nothing left, so nothing was paid. A spawn is marked failed; a
                        // standby that was never sent was never bought, and leaves the file.
                        if index == 0 {
                            members[0].state = State::Failed;
                            members[0].error =
                                Some((error.code.as_str().to_owned(), error.message.clone()));
                        } else {
                            members.pop();
                        }
                        let _ = leases.write(&members);
                    })?;
                let reply = settle_spawn(&leases, &mut members, index, answer, &ledger)?;
                if index == 0 {
                    access = reply;
                }
            }
            // The lease is live and paid for: what reaches it is printed whatever the extends do.
            extend_rounds(renter, &leases, &mut members, intervals - 1, &ledger)
        })();
        match bought {
            Ok(()) => Ok(announce(
                renter,
                &members,
                spawn_report(&members, &access, intervals),
            )),
            Err(error) => {
                let mut error = partly_paid(error, &members, &ledger);
                if ledger.bought.get() > 0 {
                    error.message.push_str(&access_text(&access));
                }
                Err(error)
            }
        }
    })
}

/// The Deployment `--deployment` names, if it does.
fn claim(args: &ComputeSpawnArgs) -> Option<Claim> {
    Some(Claim {
        environment: args.deployment.clone()?,
        repo: args.repo.clone()?,
        commit: args.commit.clone()?,
        url: args.url.clone(),
    })
}

/// Make the Deployment of the lease `members` make up, through the code `toon deployment` uses. With `probe` it
/// builds it and, with an agent key, has it signed and thrown away, and sends nothing.
fn deploy(
    renter: &Renter,
    members: &[Lease],
    claim: &Claim,
    expires_at: u64,
    probe: bool,
) -> Result<(), Error> {
    let (tags, content) = deployment::for_lease(now(), claim, members, expires_at)?;
    match renter {
        Renter::Home(_) if probe => Ok(()),
        Renter::Home(home) => {
            deployment::publish_deployment(home, &claim.environment, tags, content, None)
                .map(|_| ())
        }
        Renter::Agent(agent) => (agent.sign_deployment)(&tags, &content, !probe),
    }
}

/// Publish the Deployment a bought lease carries, with its `expires_at`. A publish that fails
/// never fails the command: the lease is bought either way, so it warns and says how to retry.
fn announce(renter: &Renter, members: &[Lease], mut report: Report) -> Report {
    let (Some(claim), Some(expires_at)) = (&members[0].deployment, members[0].expires_at) else {
        return report;
    };
    let environment = &claim.environment;
    match deploy(renter, members, claim, expires_at, false) {
        Ok(()) => {
            report.json["deployment"] = json!({ "environment": environment, "published": true });
            report
                .text
                .push_str(&format!(" Deployment {environment} is published."));
        }
        Err(error) => {
            let retry = deployment::retry_command(claim, members, expires_at);
            eprintln!(
                "warning: the lease is bought, but its Deployment {environment} was not \
                 published: {} Run `{retry}` to retry.",
                error.message
            );
            report.json["deployment"] = json!({
                "environment": environment,
                "published": false,
                "error": error.message,
                "retry": retry,
            });
        }
    }
    report
}

fn intervals_text(intervals: u64) -> String {
    match intervals {
        1 => "one Lease Interval".to_owned(),
        n => format!("{n} Lease Intervals"),
    }
}

/// The spawn's report once everything it pays is paid.
fn spawn_report(members: &[Lease], access: &Value, intervals: u64) -> Report {
    let lease = &members[0];
    let paid = paid_in_all(members);
    let set = if members.len() > 1 {
        format!("\nThe Standby Set: {}.", members_text(members))
    } else {
        String::new()
    };
    Report {
        exit: Exit::Success,
        json: json!({
            "workload_id": lease.workload_id,
            "provider": lease.provider,
            "listing": lease.listing,
            "listing_version": lease.listing_version,
            "role": lease.role,
            "expires_at": lease.expires_at,
            "access": access,
            "paid": paid,
            "lease_limit": lease.lease_limit,
            "intervals": intervals,
            "members": members_json(members),
        }),
        text: format!(
            "Lease {} is live until {} (unix time), paid {} for {}.{}{set}",
            lease.workload_id,
            lease.expires_at.unwrap_or_default(),
            amount_text(paid),
            intervals_text(intervals),
            access_text(access)
        ),
    }
}

/// Record what the answer to the spawn (or standby) packet of `members[index]` says in the
/// lease file. A lease the provider took gives back its `access`.
fn settle_spawn(
    leases: &Leases,
    members: &mut [Lease],
    index: usize,
    answer: Answer,
    ledger: &Ledger,
) -> Result<Value, Error> {
    let price = members[index].price.unwrap_or_default();
    let id = members[0].workload_id.clone();
    let what = if index == 0 {
        "the spawn".to_owned()
    } else {
        format!("the standby at {}", short(&members[index].provider))
    };
    match reply_of(answer) {
        Reply::Body(body) => {
            members[index].paid = ledger.answered(price);
            let expires_at = body["expires_at"]
                .as_u64()
                .filter(|_| body["workload_id"] == id.as_str());
            let Some(expires_at) = expires_at else {
                members[index].state = State::Unknown;
                let _ = keep(leases, members);
                return Err(failure(
                    ErrorCode::LeaseRefused,
                    format!(
                        "The provider answered {what} with something that is not a lease: \
                         {body}. The lease file {id} is marked unknown; `toon compute status \
                         {id}` settles it."
                    ),
                ));
            };
            let lease = &mut members[index];
            lease.state = State::Live;
            lease.expires_at = Some(expires_at);
            if let Some(role) = body["role"].as_str() {
                lease.role = role.to_owned();
            }
            ledger.bought_one();
            // Not a failure if the file cannot be updated: the lease is bought.
            let warning = keep(leases, members);
            if !warning.is_empty() {
                eprintln!("{}", warning.trim());
            }
            Ok(body["access"].clone())
        }
        Reply::Error { error, message } => {
            let lease = &mut members[index];
            lease.paid = ledger.answered(price);
            lease.state = State::Failed;
            lease.error = Some((error.clone(), message.clone()));
            let paid = lease.paid;
            let warning = keep(leases, members);
            Err(failure(
                ErrorCode::LeaseRefused,
                format!(
                    "The provider refused {what} with {error}: {message} It was billed all \
                     the same: {} paid. The lease file {id} is marked {}.{warning}",
                    amount_text(paid),
                    if index == 0 {
                        "failed".to_owned()
                    } else {
                        format!("failed at the standby {}", short(&members[index].provider))
                    }
                ),
            ))
        }
        Reply::Rejected { code, message } => {
            let lease = &mut members[index];
            lease.paid = ledger.unanswered(price);
            lease.state = State::Failed;
            lease.error = Some((format!("rejected {code}"), message.clone()));
            let paid = lease.paid;
            let warning = keep(leases, members);
            Err(failure(
                ErrorCode::SendFailed,
                format!(
                    "The packet was rejected with {code}: {message} {} paid. The lease file {id} \
                     is marked failed.{warning}",
                    amount_text(paid)
                ),
            ))
        }
        Reply::Unanswered | Reply::Strange(_) => {
            let lease = &mut members[index];
            lease.paid = ledger.unanswered(price);
            lease.state = State::Unknown;
            let _ = keep(leases, members);
            let mut error = operator::unanswered_cost(u128::from(ledger.paid()), None);
            error.message = format!(
                "{} The lease file {id} is marked unknown: `toon compute status {id}` asks the \
                 provider whether {what} took.",
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

/// One `.extend` packet for `members[index]`, or `.standby.extend` while it is a reserved
/// standby, recording the answer's real `expires_at` and the amount in the file. It never
/// retries a paid route by itself.
fn extend_once(
    renter: &Renter,
    leases: &Leases,
    members: &mut [Lease],
    index: usize,
    ledger: &Ledger,
) -> Result<(), Error> {
    let id = members[0].workload_id.clone();
    let price = lease_price(&members[index])?;
    let member = &members[index];
    let route = if member.role == "standby" {
        Route::StandbyExtend
    } else {
        Route::Extend
    };
    let destination = lease_request::destination(
        &member.ilp_address,
        route,
        &member.listing_name,
        member.listing_version,
    );
    let answer = renter.send(
        route,
        &destination,
        price,
        &member.seal_key()?,
        &json!({ "workload_id": id }),
    )?;
    let reply = reply_of(answer);
    let billed = match &reply {
        Reply::Body(_) | Reply::Error { .. } => ledger.answered(price),
        _ => ledger.unanswered(price),
    };
    let lease = &mut members[index];
    lease.paid += billed;
    let cause = match reply {
        Reply::Body(body) => match body["expires_at"]
            .as_u64()
            .filter(|_| body["workload_id"] == id.as_str())
        {
            Some(expires_at) => {
                ledger.bought_one();
                lease.expires_at = Some(expires_at);
                None
            }
            None => Some(failure(
                ErrorCode::LeaseRefused,
                format!(
                    "The provider answered the extend with something that is not an extension: \
                     {body}. It was billed all the same."
                ),
            )),
        },
        Reply::Error { error, message } => {
            if error == "expired" {
                lease.state = State::Ended;
            }
            Some(failure(
                ErrorCode::LeaseRefused,
                format!(
                    "The provider refused the extend with {error}: {message} It was billed all \
                     the same."
                ),
            ))
        }
        Reply::Rejected { code, message } => Some(failure(
            ErrorCode::SendFailed,
            format!("The extend was rejected with {code}: {message}"),
        )),
        Reply::Unanswered | Reply::Strange(_) => {
            Some(operator::unanswered_cost(u128::from(ledger.paid()), None))
        }
    };
    // What was bought is in the file before anything else happens.
    match (leases.write(members), cause) {
        (_, Some(cause)) => Err(cause),
        (Err(error), None) => Err(error),
        (Ok(()), None) => Ok(()),
    }
}

/// `rounds` rounds of `.extend` packets, one to each member that is live, in order. It stops
/// at the first packet that fails.
fn extend_rounds(
    renter: &Renter,
    leases: &Leases,
    members: &mut [Lease],
    rounds: u64,
    ledger: &Ledger,
) -> Result<(), Error> {
    for _ in 0..rounds {
        for index in 0..members.len() {
            if members[index].state == State::Live {
                extend_once(renter, leases, members, index, ledger)?;
            }
        }
    }
    Ok(())
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
    let mut members = leases.read(&args.workload)?;
    let live: Vec<usize> = (0..members.len())
        .filter(|index| members[*index].state == State::Live)
        .collect();
    if live.is_empty() {
        let state = members[0].state;
        return Err(usage(format!(
            "Lease {} is {}, and only a live lease is extended.{}",
            args.workload,
            state.name(),
            if state == State::Unknown {
                format!(" `toon compute status {}` settles it.", args.workload)
            } else {
                String::new()
            }
        )));
    }
    let lease_limit = args.max.or(members[0].lease_limit);
    for member in &mut members {
        member.lease_limit = lease_limit;
    }
    let (mut total, mut max_price) = (0u64, 0u64);
    for index in &live {
        let price = lease_price(&members[*index])?;
        total = total
            .checked_add(total_of(price, args.intervals)?)
            .ok_or_else(|| usage("The total is more than any amount can be."))?;
        max_price = max_price.max(price);
    }
    for index in &live {
        renter.reaches(&members[*index].ilp_address, &members[*index].connector_url)?;
    }
    limits_refuse(
        renter,
        "extend",
        lease_limit,
        paid_in_all(&members),
        max_price,
        total,
    )?;
    if !args.yes {
        return Err(unsent(
            ErrorCode::NotConfirmed,
            format!(
                "This extend pays {} for {} of lease {}{}, which is not refunded. Add `--yes` to \
                 say that you mean it.",
                amount_text(total),
                intervals_text(args.intervals),
                args.workload,
                if live.len() > 1 {
                    format!(" for each of its {} live members", live.len())
                } else {
                    String::new()
                }
            ),
        ));
    }
    // A changed lease limit is kept before anything is paid.
    leases.write(&members)?;
    paying(renter, total, |moved| {
        let ledger = Ledger::new(total, moved, args.intervals * live.len() as u64);
        match extend_rounds(renter, &leases, &mut members, args.intervals, &ledger) {
            Ok(()) => Ok(announce(
                renter,
                &members,
                extend_report(&members, args.intervals, ledger.paid()),
            )),
            Err(error) => Err(partly_paid(error, &members, &ledger)),
        }
    })
}

fn extend_report(members: &[Lease], intervals: u64, paid_now: u64) -> Report {
    let lease = &members[0];
    let set = if members.len() > 1 {
        format!("\nThe Standby Set: {}.", members_text(members))
    } else {
        String::new()
    };
    Report {
        exit: Exit::Success,
        json: json!({
            "workload_id": lease.workload_id,
            "intervals": intervals,
            "expires_at": lease.expires_at,
            "paid_now": paid_now,
            "paid": paid_in_all(members),
            "lease_limit": lease.lease_limit,
            "members": members_json(members),
        }),
        text: format!(
            "Lease {} is extended by {}, paid {}: it is live until {} (unix time), and has \
             paid {} in all.{set}",
            lease.workload_id,
            intervals_text(intervals),
            amount_text(paid_now),
            lease.expires_at.unwrap_or_default(),
            amount_text(paid_in_all(members))
        ),
    }
}

// ---- status, terminate, list, forget ----

/// What a lease's providers say about it.
struct Asked {
    report: Report,
}

fn ask(renter: &Renter, lease: &Lease, route: Route, op: &str) -> Result<Reply, Error> {
    ask_with(
        renter,
        lease,
        route,
        op,
        json!({ "workload_id": lease.workload_id }),
    )
}

fn ask_with(
    renter: &Renter,
    lease: &Lease,
    route: Route,
    op: &str,
    content: Value,
) -> Result<Reply, Error> {
    renter.reaches(&lease.ilp_address, &lease.connector_url)?;
    let request = lease_request(lease, op, content)?;
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

/// Ask one member for its status and record the answer in `lease`; the caller writes the file.
fn status_member(renter: &Renter, lease: &mut Lease) -> Result<Value, Error> {
    let was = lease.state;
    match ask(renter, lease, Route::Status, "status")? {
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
            Ok(body)
        }
        Reply::Error { error, message } => {
            match (error.as_str(), was) {
                ("unknown_workload", State::Unknown) => {
                    lease.state = State::Failed;
                    lease.error = Some((error.clone(), message.clone()));
                }
                ("expired", _) => lease.state = State::Ended,
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

fn status_of(renter: &Renter, leases: &Leases, workload: &str) -> Result<Asked, Error> {
    let mut members = leases.read(workload)?;
    let mut answers = Vec::new();
    for member in &mut members {
        answers.push(status_member(renter, member));
    }
    let written = leases.write(&members);
    if answers.iter().all(Result::is_err) {
        return Err(answers
            .into_iter()
            .next()
            .and_then(Result::err)
            .expect("a member"));
    }
    written?;
    let mut entries = Vec::new();
    let mut lines = Vec::new();
    let mut first: Option<Value> = None;
    for (member, answer) in members.iter().zip(&answers) {
        let mut entry = member_json(member);
        match answer {
            Ok(body) => {
                entry["state"] = body["state"].clone();
                entry["access"] = body["access"].clone();
                entry["takeover"] = body["takeover"].clone();
                lines.push(format!(
                    "{} {} is {}, until {} (unix time).{}",
                    member.role,
                    short(&member.provider),
                    state_text(&body["state"]),
                    member
                        .expires_at
                        .map_or("unknown".to_owned(), |at| at.to_string()),
                    access_text(&body["access"])
                ));
                first.get_or_insert_with(|| entry.clone());
            }
            Err(error) => {
                entry["error"] = json!({ "code": error.code.as_str(), "message": error.message });
                lines.push(format!(
                    "{} {} did not answer: {}",
                    member.role,
                    short(&member.provider),
                    error.message
                ));
            }
        }
        entries.push(entry);
    }
    let first = first.expect("a member answered");
    let lease = &members[0];
    let text = if members.len() == 1 {
        let body = answers[0].as_ref().expect("the member answered");
        format!(
            "Lease {} is {}, until {} (unix time).{}",
            lease.workload_id,
            state_text(&body["state"]),
            lease
                .expires_at
                .map_or("unknown".to_owned(), |at| at.to_string()),
            access_text(&body["access"])
        )
    } else {
        format!("Lease {}:\n{}", lease.workload_id, lines.join("\n"))
    };
    Ok(Asked {
        report: Report {
            exit: Exit::Success,
            json: json!({
                "workload_id": lease.workload_id,
                "role": first["role"],
                "state": first["state"],
                "expires_at": first["expires_at"],
                "access": first["access"],
                "takeover": first["takeover"],
                "members": entries,
            }),
            text,
        },
    })
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
    let mut members = leases.read(workload)?;
    if !yes {
        let latest = members.iter().filter_map(|member| member.expires_at).max();
        let left = match latest {
            Some(at) if at > now() => {
                let seconds = at - now();
                format!(
                    "the {} minute(s) paid until {at} (unix time)",
                    seconds.div_ceil(60)
                )
            }
            _ => "whatever paid time is left".to_owned(),
        };
        let of = if members.len() > 1 {
            " and of every member of its Standby Set"
        } else {
            ""
        };
        return Err(Error {
            code: ErrorCode::NotConfirmed,
            ..unsent(
                ErrorCode::NotConfirmed,
                format!(
                    "Terminating lease {workload}{of} loses {left}: nothing is refunded. Add \
                     `--yes` to say that you mean it."
                ),
            )
        });
    }
    let set = members.len() > 1;
    let mut failures: Vec<(String, Error)> = Vec::new();
    let mut entries = Vec::new();
    for member in &mut members {
        // A member of a set that was never bought, or has ended, has nothing to end.
        if set && matches!(member.state, State::Failed | State::Ended) {
            entries.push(member_json(member));
            continue;
        }
        let answer = match ask(renter, member, Route::Terminate, "terminate") {
            Ok(Reply::Body(body)) => {
                member.state = State::Ended;
                Ok(body)
            }
            Ok(Reply::Error { error, message }) => {
                if error == "expired" {
                    member.state = State::Ended;
                }
                Err(failure(
                    ErrorCode::LeaseRefused,
                    format!("The provider answered {error}: {message}"),
                ))
            }
            Ok(other) => Err(unreached(other)),
            Err(error) => Err(error),
        };
        let mut entry = member_json(member);
        match answer {
            Ok(body) => entry["state"] = body["state"].clone(),
            Err(error) => {
                entry["error"] = json!({ "code": error.code.as_str(), "message": error.message });
                failures.push((
                    format!("{} {}", member.role, short(&member.provider)),
                    error,
                ));
            }
        }
        entries.push(entry);
    }
    let written = leases.write(&members);
    if !set {
        if let Some((_, error)) = failures.pop() {
            return Err(error);
        }
        written?;
    } else if !failures.is_empty() {
        let code = failures[0].1.code;
        return Err(failure(
            code,
            format!(
                "Terminated {} of {} members of lease {workload}; {}. Run `toon compute \
                 terminate {workload} --yes` again to end the rest.",
                members.len() - failures.len(),
                members.len(),
                failures
                    .iter()
                    .map(|(who, error)| format!("{who} did not end: {}", error.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    } else {
        written?;
    }
    Ok(Report {
        exit: Exit::Success,
        json: json!({
            "workload_id": workload,
            "state": entries[0]["state"],
            "members": entries,
        }),
        text: format!("Lease {workload} is terminated; nothing is refunded."),
    })
}

/// Replace the lease's token (§6.8). The new secret is written beside the old before the first
/// packet; each member that confirms is recorded at once; the old secret goes only when every
/// member has confirmed. A member of a set that was never bought, or has ended, is passed over.
pub fn rotate(renter: &Renter, workload: &str) -> Result<Report, Error> {
    let leases = renter.leases();
    let mut members = leases.read(workload)?;
    if members[0].next_secret.is_none() {
        let next = Zeroizing::new(hex::encode(keystore::random::<32>()?));
        for lease in &mut members {
            lease.next_secret = Some(next.clone());
            lease.rotated.clear();
        }
    }
    // The new secret is in the file before anything is sent.
    leases.write(&members)?;
    let next = members[0].next_root()?.expect("a rotation is under way");
    let set = members.len() > 1;
    let mut stuck = Vec::new();
    for index in 0..members.len() {
        let lease = &mut members[index];
        let member = lease.provider.clone();
        if lease.rotated.contains(&member)
            || (set && matches!(lease.state, State::Failed | State::Ended))
        {
            continue;
        }
        let content = lease_request::rotate_content(&lease.workload_id, &next, &member);
        let reason = match ask_with(renter, lease, Route::Rotate, "rotate", content)? {
            Reply::Body(body) if lease_request::rotated(&body) => None,
            Reply::Error { error, message } if error == "not_tenant" => {
                // An earlier run may have been confirmed without our hearing it: the member
                // then knows only the new token, and reads the lease with it.
                lease.rotated.push(member.clone());
                match ask(renter, lease, Route::Status, "status")? {
                    Reply::Body(_) => None,
                    _ => {
                        lease.rotated.retain(|done| *done != member);
                        Some(format!("answered {error}: {message}"))
                    }
                }
            }
            Reply::Error { error, message } => Some(format!("answered {error}: {message}")),
            Reply::Body(_) => Some("did not confirm the rotation".to_owned()),
            Reply::Rejected { code, message } => {
                Some(format!("the packet was rejected with {code}: {message}"))
            }
            Reply::Unanswered => Some("did not answer".to_owned()),
            Reply::Strange(what) => Some(what),
        };
        match reason {
            None => {
                // Every member's copy of the lease says who holds the new token.
                for lease in &mut members {
                    if !lease.rotated.contains(&member) {
                        lease.rotated.push(member.clone());
                    }
                }
                leases.write(&members)?;
            }
            Some(reason) => stuck.push(format!("{member} {reason}")),
        }
    }
    if !stuck.is_empty() {
        return Err(failure(
            ErrorCode::RotationIncomplete,
            format!(
                "The rotation of lease {workload} is not complete: {}. The lease file keeps \
                 both secrets, and each member is presented the token it holds; run \
                 `toon compute rotate {workload}` again to finish.",
                stuck.join("; ")
            ),
        ));
    }
    for lease in &mut members {
        lease.root_secret = lease.next_secret.take().expect("a rotation is under way");
        lease.rotated.clear();
    }
    leases.write(&members)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "workload_id": workload, "rotated": true }),
        text: format!("Lease {workload} holds a new token; the old secret is gone from its file."),
    })
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
        short(&lease.provider),
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

/// The line of a member after the first, under its lease's.
fn member_row(lease: &Lease) -> String {
    format!(
        "  member {} {} {}@v{} provider {} expires {} paid {}{}",
        lease.role,
        lease.state.name(),
        lease.listing_name,
        lease.listing_version,
        short(&lease.provider),
        lease
            .expires_at
            .map_or_else(|| "-".to_owned(), |at| at.to_string()),
        lease.paid,
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
    for members in leases.list()? {
        let lease = &members[0];
        let mut entry = lease.public();
        entry["paid"] = json!(paid_in_all(&members));
        entry["members"] = Value::Array(members.iter().map(Lease::public).collect());
        let mut line = row(lease);
        if members.len() > 1 {
            line = format!("{line} (members: {})", members.len());
            for member in &members[1..] {
                line.push('\n');
                line.push_str(&member_row(member));
            }
        }
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
    let members = leases.read(workload)?;
    let may_be_live = members.iter().any(|lease| {
        matches!(lease.state, State::Live | State::Unknown)
            && lease.expires_at.is_none_or(|at| at > now())
    });
    let latest = members.iter().filter_map(|lease| lease.expires_at).max();
    leases.remove(workload)?;
    let warning = may_be_live.then(|| {
        format!(
            "The lease may still be live{}: its root secret went with the file, so nobody can \
             read, extend or end it any more.",
            latest
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
        ComputeCommand::Rotate { workload } => rotate(renter, workload),
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
