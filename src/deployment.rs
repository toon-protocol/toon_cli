//! `toon deployment`: publish and maintain a TOON Compute Deployment (kind 30437).
//!
//! The wire shape is the Deployment draft in toon-protocol/relay, `docs/nips/nip-deployment.md`
//! (cited from `nips/README.md`, not copied). A Deployment is the agent identity's signed
//! claim that one environment of one repository is served by one lease; it is optional and
//! public. Like the tip route, it goes to the agent node's own relay and to the relay the
//! network profile follows. `update` and `delete` find it on the own relay and never rebuild it
//! from flags alone.

use std::path::Path;

use serde_json::{json, Value};

use crate::apps;
use crate::cli::DeploymentCommand;
use crate::egress::Egress;
use crate::event;
use crate::home;
use crate::node::State;
use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::subscribe;
use crate::tip;

/// The kind of a Deployment: addressable by `(kind, pubkey, d)`.
const KIND: u64 = 30_437;

/// The kind of a repository announcement (NIP-34), and of a Listing.
const REPOSITORY: &str = "30617";
const LISTING: &str = "30432";

/// The kind of NIP-09's deletion request.
const DELETION: u64 = 5;

/// A Deployment that ends within this many seconds is marked expiring.
const EXPIRING: u64 = 24 * 60 * 60;

type Tags = Vec<Vec<String>>;

fn refused(message: impl Into<String>) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::DeploymentRefused,
        message: format!("{} Nothing was sent.", message.into()),
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A provider's key as 64 lower-case hex characters, from hex or an `npub`.
fn provider(flag: &str, text: &str) -> Result<String, Error> {
    let key = if text.starts_with("npub1") {
        tip::npub_key(text)
    } else {
        Some(text.to_owned())
    };
    key.filter(|key| is_hex(key, 64)).ok_or_else(|| {
        refused(format!(
            "{flag} {text:?} is not a public key: 64 lower-case hex characters or an npub."
        ))
    })
}

/// `kind:pubkey:identifier` of the given kind.
fn coordinate(flag: &str, kind: &str, noun: &str, text: &str) -> Result<String, Error> {
    let mut parts = text.splitn(3, ':');
    let valid = parts.next() == Some(kind)
        && parts.next().is_some_and(|key| is_hex(key, 64))
        && parts.next().is_some_and(|name| !name.is_empty());
    if valid {
        Ok(text.to_owned())
    } else {
        Err(refused(format!(
            "{flag} {text:?} is not {noun}: it must be `{kind}:<pubkey, 64 hex>:<name>`."
        )))
    }
}

fn commit(text: &str) -> Result<String, Error> {
    if is_hex(text, 40) || is_hex(text, 64) {
        Ok(text.to_owned())
    } else {
        Err(refused(format!(
            "--commit {text:?} is not a full commit id: 40 or 64 lower-case hex characters."
        )))
    }
}

fn workload(text: &str) -> Result<String, Error> {
    if is_hex(text, 64) {
        Ok(text.to_owned())
    } else {
        Err(refused(format!(
            "--workload {text:?} is not a workload id: 64 lower-case hex characters."
        )))
    }
}

fn image(text: &str) -> Result<String, Error> {
    let digest = text.strip_prefix("sha256:").unwrap_or(text);
    if is_hex(digest, 64) {
        Ok(digest.to_owned())
    } else {
        Err(refused(format!(
            "--image {text:?} is not an image digest: 64 lower-case hex characters, with or \
             without a leading `sha256:`."
        )))
    }
}

fn url(text: &str) -> Result<String, Error> {
    let rest = text.strip_prefix("https://").unwrap_or_default();
    if rest.is_empty() || rest.starts_with('/') || text.chars().any(char::is_whitespace) {
        Err(refused(format!(
            "--url {text:?} is not an https URL: the `r` tag is where people open the workload."
        )))
    } else {
        Ok(text.to_owned())
    }
}

/// The expiration the flags give, checked against the lease's `expires_at` and the clock.
fn expiration(now: u64, expires_at: u64, expiration: Option<u64>) -> Result<u64, Error> {
    let expiration = expiration.unwrap_or(expires_at);
    if expires_at <= now {
        return Err(refused(format!(
            "--expires-at {expires_at} is in the past: the lease has ended."
        )));
    }
    if expiration <= now {
        return Err(refused(format!(
            "--expiration {expiration} is in the past."
        )));
    }
    if expiration > expires_at {
        return Err(refused(format!(
            "--expiration {expiration} is later than --expires-at {expires_at}: a Deployment \
             cannot outlive its lease."
        )));
    }
    Ok(expiration)
}

fn standbys(primary: &str, standby: &[String]) -> Result<Vec<String>, Error> {
    let mut keys: Vec<String> = Vec::new();
    for text in standby {
        let key = provider("--standby", text)?;
        if key == primary || keys.contains(&key) {
            return Err(refused(format!(
                "--standby {text:?} names a provider the Deployment already names."
            )));
        }
        keys.push(key);
    }
    Ok(keys)
}

fn tag(name: &str, values: &[&str]) -> Vec<String> {
    std::iter::once(name)
        .chain(values.iter().copied())
        .map(str::to_owned)
        .collect()
}

fn provider_tags(primary: &str, standby: &[String]) -> Tags {
    let mut tags = vec![tag("p", &[primary, "primary"])];
    tags.extend(standby.iter().map(|key| tag("p", &[key, "standby"])));
    tags
}

/// Everything `publish` is told, checked, with the tags in the order the NIP lists them.
struct Inputs<'a> {
    environment: &'a str,
    repo: &'a str,
    commit: &'a str,
    listing: &'a str,
    primary: &'a str,
    standby: &'a [String],
    workload: &'a str,
    image: Option<&'a str>,
    url: Option<&'a str>,
    expires_at: u64,
    expiration: Option<u64>,
    note: &'a str,
}

/// The tags and content of a new Deployment, or what is wrong with the inputs.
fn build(now: u64, inputs: &Inputs) -> Result<(Tags, String), Error> {
    if inputs.environment.trim().is_empty() || inputs.environment.contains('\n') {
        return Err(refused(
            "The environment must be one non-empty line: it is the Deployment's `d` tag.",
        ));
    }
    let primary = provider("--primary", inputs.primary)?;
    let standby = standbys(&primary, inputs.standby)?;
    let mut tags = vec![
        tag("d", &[inputs.environment]),
        tag(
            "a",
            &[&coordinate(
                "--repo",
                REPOSITORY,
                "a repository address",
                inputs.repo,
            )?],
        ),
        tag("c", &[&commit(inputs.commit)?]),
        tag(
            "a",
            &[&coordinate(
                "--listing",
                LISTING,
                "a Listing address",
                inputs.listing,
            )?],
        ),
    ];
    tags.extend(provider_tags(&primary, &standby));
    tags.push(tag("x", &[&workload(inputs.workload)?, "workload"]));
    if let Some(digest) = inputs.image {
        tags.push(tag("x", &[&image(digest)?, "image"]));
    }
    if let Some(address) = inputs.url {
        tags.push(tag("r", &[&url(address)?]));
    }
    let expiration = expiration(now, inputs.expires_at, inputs.expiration)?;
    tags.push(tag("expiration", &[&expiration.to_string()]));
    tags.push(tag("L", &["toon.network"]));
    Ok((tags, inputs.note.to_owned()))
}

/// What `update` was told to change. A group that is `None` or empty is left as it is.
#[derive(Default)]
struct Changes<'a> {
    expires_at: Option<u64>,
    expiration: Option<u64>,
    primary: Option<&'a str>,
    standby: &'a [String],
    listing: Option<&'a str>,
    commit: Option<&'a str>,
    image: Option<&'a str>,
    note: Option<&'a str>,
}

/// Replace the tags `matches` selects by `replacement`, at the place of the first of them,
/// or before `expiration` when there is none.
fn replace(tags: &mut Tags, matches: impl Fn(&[String]) -> bool, replacement: Tags) {
    let at = tags.iter().position(|tag| matches(tag));
    tags.retain(|tag| !matches(tag));
    let at = at.unwrap_or_else(|| {
        tags.iter()
            .position(|tag| tag[0] == "expiration")
            .unwrap_or(tags.len())
    });
    for (offset, tag) in replacement.into_iter().enumerate() {
        tags.insert(at + offset, tag);
    }
}

fn marked(name: &'static str, marker: &'static str) -> impl Fn(&[String]) -> bool {
    move |tag| tag[0] == name && tag.get(2).map(String::as_str) == Some(marker)
}

fn values<'a>(tags: &'a Tags, name: &str, marker: &str) -> Vec<&'a str> {
    tags.iter()
        .filter(|tag| tag[0] == name && tag.get(2).map(String::as_str) == Some(marker))
        .map(|tag| tag[1].as_str())
        .collect()
}

/// The tags and content of the Deployment `current` after `changes`; everything the changes
/// do not name is carried over.
fn amend(now: u64, current: &Value, changes: &Changes) -> Result<(Tags, String), Error> {
    let mut tags: Tags = current["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    tag.as_array()?
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .filter(|tag| !tag.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if let Some(expires_at) = changes.expires_at {
        let new = expiration(now, expires_at, changes.expiration)?;
        replace(
            &mut tags,
            |tag| tag[0] == "expiration",
            vec![tag("expiration", &[&new.to_string()])],
        );
    }
    if changes.primary.is_some() || !changes.standby.is_empty() {
        let old_primary = values(&tags, "p", "primary")
            .first()
            .map(|key| (*key).to_owned());
        let primary = match changes.primary {
            Some(text) => provider("--primary", text)?,
            None => old_primary.ok_or_else(|| {
                refused("The current Deployment names no primary; give --primary.")
            })?,
        };
        let standby = if changes.standby.is_empty() {
            // A standby that takes over is the primary now, not a standby too.
            values(&tags, "p", "standby")
                .into_iter()
                .filter(|key| *key != primary)
                .map(str::to_owned)
                .collect()
        } else {
            changes.standby.to_vec()
        };
        let standby = standbys(&primary, &standby)?;
        replace(
            &mut tags,
            |tag| tag[0] == "p",
            provider_tags(&primary, &standby),
        );
    }
    if let Some(text) = changes.listing {
        let listing = coordinate("--listing", LISTING, "a Listing address", text)?;
        replace(
            &mut tags,
            |tag| {
                tag[0] == "a"
                    && tag
                        .get(1)
                        .is_some_and(|a| a.starts_with(&format!("{LISTING}:")))
            },
            vec![tag("a", &[&listing])],
        );
    }
    if let Some(text) = changes.commit {
        replace(
            &mut tags,
            |tag| tag[0] == "c",
            vec![tag("c", &[&commit(text)?])],
        );
    }
    if let Some(text) = changes.image {
        replace(
            &mut tags,
            marked("x", "image"),
            vec![tag("x", &[&image(text)?, "image"])],
        );
    }
    let content = changes
        .note
        .map_or_else(
            || current["content"].as_str().unwrap_or_default(),
            |note| note,
        )
        .to_owned();
    check(now, &tags)?;
    Ok((tags, content))
}

/// Whether tags keep the NIP's shape, whatever the current event carried over.
fn check(now: u64, tags: &Tags) -> Result<(), Error> {
    let count = |name: &str| tags.iter().filter(|tag| tag[0] == name).count();
    let primaries = values(tags, "p", "primary").len();
    if primaries != 1 {
        return Err(refused(format!(
            "A Deployment names exactly one primary; this one would have {primaries}."
        )));
    }
    if tags.iter().any(|tag| {
        tag[0] == "p" && !matches!(tag.get(2).map(String::as_str), Some("primary" | "standby"))
    }) {
        return Err(refused("A `p` tag must be marked `primary` or `standby`."));
    }
    if tags.iter().any(|tag| {
        tag[0] == "x" && !matches!(tag.get(2).map(String::as_str), Some("workload" | "image"))
    }) {
        return Err(refused("An `x` tag must be marked `workload` or `image`."));
    }
    if values(tags, "x", "workload").len() != 1 || values(tags, "x", "image").len() > 1 {
        return Err(refused(
            "A Deployment has one `x` tag marked `workload` and at most one marked `image`.",
        ));
    }
    if count("d") != 1 || count("c") != 1 {
        return Err(refused("A Deployment has exactly one `d` and one `c` tag."));
    }
    let expirations: Vec<&Vec<String>> = tags.iter().filter(|tag| tag[0] == "expiration").collect();
    let valid = expirations.len() == 1
        && expirations[0]
            .get(1)
            .and_then(|text| text.parse::<u64>().ok())
            .is_some_and(|at| at > now);
    if !valid {
        return Err(refused(
            "A Deployment needs one `expiration` in the future. Extend the lease and pass \
             --expires-at.",
        ));
    }
    Ok(())
}

fn tag_value<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == name)
        .and_then(|tag| tag[1].as_str())
}

fn repository_of(event: &Value) -> Option<&str> {
    event["tags"]
        .as_array()?
        .iter()
        .filter(|tag| tag[0] == "a")
        .filter_map(|tag| tag[1].as_str())
        .find(|a| a.starts_with(&format!("{REPOSITORY}:")))
}

fn primary_of(event: &Value) -> Option<&str> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == "p" && tag[2] == "primary")
        .and_then(|tag| tag[1].as_str())
}

/// The newest of the Deployments with this `d` by this author: the greatest `created_at`, and
/// of two the same, the lower id.
fn newest<'a>(events: &'a [Value], author: &str, environment: &str) -> Option<&'a Value> {
    events
        .iter()
        .filter(|event| {
            event["kind"] == KIND
                && event["pubkey"] == author
                && tag_value(event, "d") == Some(environment)
        })
        .min_by(|a, b| {
            let created = |event: &Value| event["created_at"].as_u64().unwrap_or(0);
            let id = |event: &'a Value| event["id"].as_str().unwrap_or_default();
            created(b).cmp(&created(a)).then_with(|| id(a).cmp(id(b)))
        })
}

/// `toon deployment`.
pub fn run(command: DeploymentCommand) -> Result<Report, Error> {
    let now = event::now();
    match command {
        DeploymentCommand::Publish {
            environment,
            repo,
            commit,
            listing,
            primary,
            standby,
            workload,
            image,
            url,
            expires_at,
            expiration,
            note,
            relay,
        } => {
            let (tags, content) = build(
                now,
                &Inputs {
                    environment: &environment,
                    repo: &repo,
                    commit: &commit,
                    listing: &listing,
                    primary: &primary,
                    standby: &standby,
                    workload: &workload,
                    image: image.as_deref(),
                    url: url.as_deref(),
                    expires_at,
                    expiration,
                    note: &note,
                },
            )?;
            let home = home::resolve()?;
            publish_deployment(&home, &environment, tags, content, relay.as_deref())
        }
        DeploymentCommand::Update {
            environment,
            expires_at,
            expiration,
            primary,
            standby,
            listing,
            commit,
            image,
            note,
            relay,
        } => {
            let changes = Changes {
                expires_at,
                expiration,
                primary: primary.as_deref(),
                standby: &standby,
                listing: listing.as_deref(),
                commit: commit.as_deref(),
                image: image.as_deref(),
                note: note.as_deref(),
            };
            if expires_at.is_none()
                && primary.is_none()
                && standby.is_empty()
                && listing.is_none()
                && commit.is_none()
                && image.is_none()
                && note.is_none()
            {
                return Err(refused(
                    "Name what changed: --expires-at, --primary, --standby, --listing, \
                     --commit, --image or --note.",
                ));
            }
            let home = home::resolve()?;
            update_deployment(&home, &environment, &changes, relay.as_deref(), now)
        }
        DeploymentCommand::Delete { environment, relay } => {
            let home = home::resolve()?;
            delete(&home, &environment, relay.as_deref(), now)
        }
        DeploymentCommand::List => list(&home::resolve()?, now),
    }
}

/// Where the second copy goes: `--relay`, else the relay the network profile follows.
fn second_relay(state: &State, named: Option<&str>) -> Result<String, Error> {
    named
        .map(str::to_owned)
        .or_else(|| state.relay_url.clone())
        .ok_or_else(|| Error {
            nothing_sent: true,
            unanswered: None,
            code: ErrorCode::NoProfileRelay,
            message: format!(
                "The network profile {} follows no relay, so there is nowhere but this agent \
                 node's own relay to publish a Deployment to, and nobody would find it. Name \
                 one with --relay.",
                state.network.name()
            ),
        })
}

/// The Deployments of `author` on the agent node's own relay.
fn held(home: &Path, state: &State, author: &str) -> Result<Vec<Value>, Error> {
    let app = subscribe::own_relay_app(state)?;
    let address = subscribe::own_relay_address(home, &app.name, subscribe::RelayPort::Read)?;
    let events = event::fetch(
        &Egress::of(home)?,
        &format!("ws://{address}"),
        &json!({ "kinds": [KIND], "authors": [author] }),
    )?;
    Ok(events
        .into_iter()
        .filter(|event| event::verify(event).is_ok())
        .collect())
}

fn current(home: &Path, state: &State, author: &str, environment: &str) -> Result<Value, Error> {
    let events = held(home, state, author)?;
    newest(&events, author, environment)
        .cloned()
        .ok_or_else(|| {
            refused(format!(
                "The agent node's own relay holds no current Deployment of {environment:?} signed \
             by this agent identity. `toon deployment publish` makes one; an update never \
             builds one from flags alone."
            ))
        })
}

fn publish_deployment(
    home: &Path,
    environment: &str,
    tags: Tags,
    content: String,
    relay: Option<&str>,
) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let second = second_relay(&state, relay)?;
    let secret = event::agent_secret(home)?;
    let author = event::public_key(&secret)?;
    let mut created_at = event::now();
    // A Deployment that replaces another is made later than it, or a relay keeps the old one.
    if let Ok(events) = held(home, &state, &author) {
        if let Some(old) = newest(&events, &author, environment) {
            created_at = created_at.max(old["created_at"].as_u64().unwrap_or(0) + 1);
        }
    }
    sign_and_publish(
        home,
        &secret,
        created_at,
        tags,
        &content,
        &second,
        "published",
    )
}

fn sign_and_publish(
    home: &Path,
    secret: &[u8; 32],
    created_at: u64,
    tags: Tags,
    content: &str,
    second: &str,
    outcome: &str,
) -> Result<Report, Error> {
    let signed = event::sign(secret, created_at, KIND, json!(tags), content)?;
    let published = tip::publish(home, second, &signed).map_err(|mut error| {
        error.message = format!(
            "The Deployment is signed but not published everywhere: {} Run the command again.",
            error.message
        );
        error
    })?;
    let id = signed["id"].as_str().unwrap_or_default().to_owned();
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "outcome": outcome, "event": signed, "published": published }),
        text: format!(
            "Deployment {id} is {outcome}: to the agent node's own relay and to {second}."
        ),
    })
}

fn update_deployment(
    home: &Path,
    environment: &str,
    changes: &Changes,
    relay: Option<&str>,
    now: u64,
) -> Result<Report, Error> {
    // Malformed flags are refused before the relay is asked.
    check_flags(now, changes)?;
    let state = apps::loaded(home)?;
    let second = second_relay(&state, relay)?;
    let secret = event::agent_secret(home)?;
    let author = event::public_key(&secret)?;
    let old = current(home, &state, &author, environment)?;
    let (tags, content) = amend(now, &old, changes)?;
    let created_at = now.max(old["created_at"].as_u64().unwrap_or(0) + 1);
    sign_and_publish(
        home, &secret, created_at, tags, &content, &second, "updated",
    )
}

/// The syntax of the flags alone, without a Deployment to apply them to.
fn check_flags(now: u64, changes: &Changes) -> Result<(), Error> {
    if let Some(expires_at) = changes.expires_at {
        expiration(now, expires_at, changes.expiration)?;
    }
    if let Some(text) = changes.primary {
        provider("--primary", text)?;
    }
    for text in changes.standby {
        provider("--standby", text)?;
    }
    if let Some(text) = changes.listing {
        coordinate("--listing", LISTING, "a Listing address", text)?;
    }
    if let Some(text) = changes.commit {
        commit(text)?;
    }
    if let Some(text) = changes.image {
        image(text)?;
    }
    Ok(())
}

/// A NIP-09 deletion request's tags: the Deployment's address, and its kind.
fn deletion_tags(address: &str) -> Value {
    json!([["a", address], ["k", KIND.to_string()]])
}

fn delete(home: &Path, environment: &str, relay: Option<&str>, now: u64) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let second = second_relay(&state, relay)?;
    let secret = event::agent_secret(home)?;
    let author = event::public_key(&secret)?;
    let old = current(home, &state, &author, environment)?;
    let address = format!("{KIND}:{author}:{environment}");
    let created_at = now.max(old["created_at"].as_u64().unwrap_or(0) + 1);
    let signed = event::sign(&secret, created_at, DELETION, deletion_tags(&address), "")?;
    let published = tip::publish(home, &second, &signed).map_err(|mut error| {
        error.message = format!(
            "The deletion is signed but not published everywhere: {} Run the command again.",
            error.message
        );
        error
    })?;
    let id = signed["id"].as_str().unwrap_or_default().to_owned();
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "outcome": "deleted", "address": address, "event": signed,
                      "published": published }),
        text: format!(
            "Deployment {address} is taken down: deletion {id} is published to the agent \
             node's own relay and to {second}."
        ),
    })
}

fn list(home: &Path, now: u64) -> Result<Report, Error> {
    let state = apps::loaded(home)?;
    let secret = event::agent_secret(home)?;
    let author = event::public_key(&secret)?;
    let events = held(home, &state, &author)?;
    let mut environments: Vec<&str> = events
        .iter()
        .filter(|event| event["kind"] == KIND && event["pubkey"] == author.as_str())
        .filter_map(|event| tag_value(event, "d"))
        .collect();
    environments.sort_unstable();
    environments.dedup();

    let mut deployments = Vec::new();
    let mut lines = Vec::new();
    for environment in environments {
        let Some(deployment) = newest(&events, &author, environment) else {
            continue;
        };
        let Some(ends) =
            tag_value(deployment, "expiration").and_then(|text| text.parse::<u64>().ok())
        else {
            continue;
        };
        if ends <= now {
            continue;
        }
        let left = ends - now;
        let expiring = left <= EXPIRING;
        let repository = repository_of(deployment).unwrap_or_default();
        let primary = primary_of(deployment).unwrap_or_default();
        deployments.push(json!({
            "environment": environment,
            "repository": repository,
            "primary": primary,
            "expiration": ends,
            "seconds_left": left,
            "expiring": expiring,
        }));
        lines.push(format!(
            "{environment}  {repository}  primary {primary}  {} left{}",
            duration(left),
            if expiring { "  EXPIRING" } else { "" }
        ));
    }
    let text = if lines.is_empty() {
        "No Deployments on the agent node's own relay.".to_owned()
    } else {
        lines.join("\n")
    };
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "deployments": deployments }),
        text,
    })
}

fn duration(seconds: u64) -> String {
    match seconds {
        0..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_762_000_000;
    const REPO: &str =
        "30617:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:toon";
    const LIST: &str =
        "30432:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb:small";
    const P1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const P2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const P3: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const W: &str = "4444444444444444444444444444444444444444444444444444444444444444";
    const IMG: &str = "5555555555555555555555555555555555555555555555555555555555555555";
    const COMMIT: &str = "6666666666666666666666666666666666666666";

    fn inputs<'a>(standby: &'a [String]) -> Inputs<'a> {
        Inputs {
            environment: "production",
            repo: REPO,
            commit: COMMIT,
            listing: LIST,
            primary: P1,
            standby,
            workload: W,
            image: Some(IMG),
            url: Some("https://abc.gw.example.com"),
            expires_at: NOW + 1000,
            expiration: None,
            note: "",
        }
    }

    fn event_of(tags: Tags, content: &str) -> Value {
        json!({ "kind": KIND, "tags": tags, "content": content, "created_at": NOW })
    }

    fn first(standby: &[String]) -> Value {
        let (tags, content) = build(NOW, &inputs(standby)).unwrap();
        event_of(tags, &content)
    }

    #[test]
    fn a_deployment_has_every_tag_the_nip_lists_in_its_order() {
        let standby = [P2.to_owned()];
        let (tags, content) = build(NOW, &inputs(&standby)).unwrap();
        assert_eq!(content, "");
        assert_eq!(
            json!(tags),
            json!([
                ["d", "production"],
                ["a", REPO],
                ["c", COMMIT],
                ["a", LIST],
                ["p", P1, "primary"],
                ["p", P2, "standby"],
                ["x", W, "workload"],
                ["x", IMG, "image"],
                ["r", "https://abc.gw.example.com"],
                ["expiration", (NOW + 1000).to_string()],
                ["L", "toon.network"]
            ])
        );
    }

    #[test]
    fn each_malformed_input_is_refused_naming_the_problem() {
        let refuse = |change: &dyn Fn(&mut Inputs)| {
            let standby = [];
            let mut inputs = inputs(&standby);
            change(&mut inputs);
            build(NOW, &inputs).expect_err("refused").message
        };
        assert!(refuse(&|i| i.expiration = Some(NOW + 1001)).contains("later than"));
        assert!(refuse(&|i| i.expiration = Some(NOW - 1)).contains("past"));
        assert!(refuse(&|i| i.expires_at = NOW - 1).contains("past"));
        assert!(refuse(&|i| i.url = Some("http://x.example")).contains("https"));
        assert!(refuse(&|i| i.workload = "abc").contains("--workload"));
        assert!(refuse(&|i| i.image = Some("abc")).contains("--image"));
        assert!(refuse(&|i| i.commit = "abc").contains("--commit"));
        assert!(refuse(&|i| i.repo = LIST).contains("--repo"));
        assert!(refuse(&|i| i.listing = REPO).contains("--listing"));
        assert!(refuse(&|i| i.primary = "nobody").contains("--primary"));
    }

    #[test]
    fn a_standby_may_not_repeat_the_primary_so_there_is_one_primary() {
        let standby = [P1.to_owned()];
        assert!(build(NOW, &inputs(&standby)).is_err());
    }

    #[test]
    fn an_image_may_drop_its_prefix_and_expiration_may_be_earlier() {
        let standby = [];
        let mut given = inputs(&standby);
        let prefixed = format!("sha256:{IMG}");
        given.image = Some(&prefixed);
        given.expiration = Some(NOW + 10);
        let (tags, _) = build(NOW, &given).unwrap();
        assert!(tags.contains(&tag("x", &[IMG, "image"])));
        assert!(tags.contains(&tag("expiration", &[&(NOW + 10).to_string()])));
    }

    fn differences(before: &Value, after: &Tags) -> Vec<Vec<String>> {
        let before: Tags = serde_json::from_value(before["tags"].clone()).unwrap();
        let mut changed: Tags = before
            .iter()
            .filter(|t| !after.contains(t))
            .cloned()
            .collect();
        changed.extend(after.iter().filter(|t| !before.contains(t)).cloned());
        changed
    }

    #[test]
    fn an_extension_changes_only_the_expiration() {
        let old = first(&[]);
        let changes = Changes {
            expires_at: Some(NOW + 5000),
            ..Changes::default()
        };
        let (tags, content) = amend(NOW, &old, &changes).unwrap();
        assert_eq!(content, "");
        assert_eq!(
            differences(&old, &tags),
            vec![
                tag("expiration", &[&(NOW + 1000).to_string()]),
                tag("expiration", &[&(NOW + 5000).to_string()])
            ]
        );
    }

    #[test]
    fn a_takeover_changes_only_the_provider_tags_and_the_listing() {
        let old = first(&[P2.to_owned()]);
        let new_listing = format!("30432:{P3}:big");
        let changes = Changes {
            primary: Some(P2),
            standby: &[P3.to_owned()],
            listing: Some(&new_listing),
            ..Changes::default()
        };
        let (tags, _) = amend(NOW, &old, &changes).unwrap();
        let changed = differences(&old, &tags);
        assert!(changed
            .iter()
            .all(|tag| tag[0] == "p" || (tag[0] == "a" && tag[1].starts_with("30432:"))));
        assert_eq!(values(&tags, "p", "primary"), vec![P2]);
        assert_eq!(values(&tags, "p", "standby"), vec![P3]);
        assert!(tags.contains(&tag("a", &[&new_listing])));
    }

    #[test]
    fn a_takeover_by_a_standby_leaves_it_out_of_the_standbys() {
        let old = first(&[P2.to_owned(), P3.to_owned()]);
        let changes = Changes {
            primary: Some(P2),
            ..Changes::default()
        };
        let (tags, _) = amend(NOW, &old, &changes).unwrap();
        assert_eq!(values(&tags, "p", "primary"), vec![P2]);
        assert_eq!(values(&tags, "p", "standby"), vec![P3]);
    }

    #[test]
    fn an_unmarked_workload_tag_is_refused() {
        let mut old = first(&[]);
        old["tags"]
            .as_array_mut()
            .unwrap()
            .push(json!(["x", "9".repeat(64)]));
        let changes = Changes {
            commit: Some(&"7".repeat(40)),
            ..Changes::default()
        };
        let error = amend(NOW, &old, &changes).unwrap_err();
        assert!(
            error.message.contains("`x` tag must be marked"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_deletion_names_the_deployments_address() {
        let address = format!("{KIND}:{P1}:production");
        assert_eq!(
            deletion_tags(&address),
            json!([["a", format!("30437:{P1}:production")], ["k", "30437"]])
        );
    }

    #[test]
    fn a_new_build_changes_only_the_commit_and_the_image() {
        let old = first(&[]);
        let commit = "7".repeat(40);
        let changes = Changes {
            commit: Some(&commit),
            image: Some(&"8".repeat(64)),
            ..Changes::default()
        };
        let (tags, _) = amend(NOW, &old, &changes).unwrap();
        let changed = differences(&old, &tags);
        assert_eq!(changed.len(), 4);
        assert!(changed.iter().all(|tag| tag[0] == "c" || tag[0] == "x"));
        assert_eq!(values(&tags, "x", "workload"), vec![W]);
    }

    #[test]
    fn a_note_is_carried_over_and_cleared_only_by_an_empty_note() {
        let mut old = first(&[]);
        old["content"] = json!("hello");
        let extension = Changes {
            expires_at: Some(NOW + 5000),
            ..Changes::default()
        };
        assert_eq!(amend(NOW, &old, &extension).unwrap().1, "hello");
        let clear = Changes {
            note: Some(""),
            ..Changes::default()
        };
        assert_eq!(amend(NOW, &old, &clear).unwrap().1, "");
    }

    #[test]
    fn an_update_that_leaves_a_lapsed_expiration_is_refused() {
        let old = first(&[]);
        let later = NOW + 2000;
        let changes = Changes {
            commit: Some(COMMIT),
            ..Changes::default()
        };
        assert!(amend(later, &old, &changes).is_err());
    }

    #[test]
    fn the_newest_deployment_of_an_environment_is_the_one_found() {
        let make = |id: &str, at: u64, d: &str| json!({ "id": id, "pubkey": "a", "kind": KIND, "created_at": at, "tags": [["d", d]] });
        let events = [
            make("b", 5, "x"),
            make("c", 7, "x"),
            make("a", 7, "x"),
            make("d", 9, "y"),
        ];
        assert_eq!(newest(&events, "a", "x").unwrap()["id"], "a");
        assert!(newest(&events, "z", "x").is_none());
    }
}
