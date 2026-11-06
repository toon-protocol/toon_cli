//! Finding a Listing and its provider's Profile on the relays (spec §4.1, §4.2).
//!
//! A tenant never trusts a relay: every event is checked against its signature and the
//! author the coordinate names. The newest version of a Listing wins, and a Listing its
//! author withdrew is not for sale. The spec names no withdrawal, so this reads the two
//! signs the Nostr conventions give: a NIP-09 deletion (kind 5) of the coordinate by the
//! author, at or after the Listing's `created_at`, and a Listing with no content.

use serde_json::{json, Value};

use crate::egress::Egress;
use crate::event;
use crate::nip19;
use crate::outcome::{Error, ErrorCode};

pub const LISTING_KIND: u64 = 30432;
pub const PROFILE_KIND: u64 = 10432;

/// A Listing's coordinate, `30432:<author>:<d>`, and the relays that may hold it.
#[derive(Debug, PartialEq, Eq)]
pub struct Coordinate {
    pub author: String,
    pub name: String,
    pub relays: Vec<String>,
}

impl Coordinate {
    pub fn text(&self) -> String {
        format!("{LISTING_KIND}:{}:{}", self.author, self.name)
    }
}

fn not_found(code: ErrorCode, message: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code,
        message,
    }
}

fn usage(message: String) -> Error {
    not_found(ErrorCode::Usage, message)
}

fn is_key(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The Listing `text` names: an `naddr` of kind 30432, or `30432:<pubkey>:<d>`, where `d`
/// may hold colons.
pub fn parse_coordinate(text: &str) -> Result<Coordinate, Error> {
    if let Some(address) = nip19::naddr(text) {
        if u64::from(address.kind) != LISTING_KIND {
            return Err(usage(format!(
                "That naddr names a kind {} event, not a Listing (kind {LISTING_KIND}).",
                address.kind
            )));
        }
        return Ok(Coordinate {
            author: address.author,
            name: address.identifier,
            relays: address.relays,
        });
    }
    let mut parts = text.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(kind), Some(author), Some(name))
            if kind == LISTING_KIND.to_string() && is_key(author) && !name.is_empty() =>
        {
            Ok(Coordinate {
                author: author.to_owned(),
                name: name.to_owned(),
                relays: Vec::new(),
            })
        }
        _ => Err(usage(format!(
            "{text} is not a Listing: give an `naddr` or `{LISTING_KIND}:<provider pubkey, 64 \
             lowercase hex characters>:<listing name>`."
        ))),
    }
}

/// What a tenant reads from a Listing.
#[derive(Debug)]
pub struct Listing {
    pub name: String,
    pub version: u64,
    pub price: u64,
}

/// What a tenant reads from a Provider Profile.
#[derive(Debug)]
pub struct Profile {
    pub ilp_address: String,
    pub connector_url: String,
    pub connector_seal_key: [u8; 65],
}

#[derive(Debug)]
pub struct Resolved {
    pub listing: Listing,
    pub profile: Profile,
}

fn content(event: &Value) -> Option<Value> {
    serde_json::from_str::<Value>(event["content"].as_str()?)
        .ok()
        .filter(Value::is_object)
}

fn tag_value<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    event["tags"]
        .as_array()?
        .iter()
        .find(|tag| tag[0] == name)
        .and_then(|tag| tag[1].as_str())
}

/// Whether `event` is a validly signed event of `kind` by `author`.
fn authentic(event: &Value, kind: u64, author: &str) -> bool {
    event["kind"].as_u64() == Some(kind)
        && event["pubkey"].as_str() == Some(author)
        && event::verify(event).is_ok()
}

/// The `connector_seal_key`: 65 bytes of hex beginning `04`, with or without `0x`.
pub fn seal_key(text: &str) -> Option<[u8; 65]> {
    hex::decode(text.strip_prefix("0x").unwrap_or(text))
        .ok()
        .and_then(|key| key.try_into().ok())
        .filter(|key: &[u8; 65]| key[0] == 0x04)
}

fn profile_of(event: &Value) -> Option<Profile> {
    let content = content(event)?;
    let text = |field: &str| {
        content[field]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(Profile {
        ilp_address: text("ilp_address")?,
        connector_url: text("connector_url")?,
        connector_seal_key: seal_key(&text("connector_seal_key")?)?,
    })
}

fn listing_of(event: &Value, name: &str) -> Option<Listing> {
    let content = content(event)?;
    Some(Listing {
        name: name.to_owned(),
        version: content["version"].as_u64()?,
        price: content["price"].as_u64()?,
    })
}

/// Read the Listing at `coordinate` and its author's Profile from `relays`: the newest
/// version of the Listing, refused when withdrawn, and the newest Profile.
pub fn resolve(
    egress: &Egress,
    relays: &[String],
    coordinate: &Coordinate,
) -> Result<Resolved, Error> {
    let (author, name) = (&coordinate.author, &coordinate.name);
    let filter = json!({ "kinds": [LISTING_KIND], "authors": [author], "#d": [name] });
    let profile_filter = json!({ "kinds": [PROFILE_KIND], "authors": [author] });
    let withdrawn_filter = json!({ "kinds": [5], "authors": [author], "#a": [coordinate.text()] });
    let (mut listings, mut profiles, mut deletions) = (Vec::new(), Vec::new(), Vec::new());
    let mut unread = Vec::new();
    let mut answered = false;
    for relay in relays {
        let read = || -> Result<_, Error> {
            Ok((
                event::fetch(egress, relay, &filter)?,
                event::fetch(egress, relay, &profile_filter)?,
                event::fetch(egress, relay, &withdrawn_filter)?,
            ))
        };
        match read() {
            Ok((found, profile, deleted)) => {
                answered = true;
                listings.extend(found);
                profiles.extend(profile);
                deletions.extend(deleted);
            }
            Err(error) => unread.push(format!("{relay}: {}", error.message)),
        }
    }
    if !answered {
        return Err(not_found(
            ErrorCode::QueryFailed,
            format!(
                "No relay could be read for the Listing {}: {}",
                coordinate.text(),
                unread.join(" ")
            ),
        ));
    }
    let newest = listings
        .into_iter()
        .filter(|event| {
            authentic(event, LISTING_KIND, author) && tag_value(event, "d") == Some(name)
        })
        .max_by_key(|event| {
            (
                content(event).and_then(|content| content["version"].as_u64()),
                event["created_at"].as_u64(),
            )
        })
        .ok_or_else(|| {
            not_found(
                ErrorCode::ListingNotFound,
                format!(
                    "No validly signed Listing {} is on the relays read: {}.",
                    coordinate.text(),
                    relays.join(", ")
                ),
            )
        })?;
    let created = newest["created_at"].as_u64().unwrap_or(0);
    let deleted = deletions.iter().any(|event| {
        authentic(event, 5, author) && event["created_at"].as_u64().unwrap_or(0) >= created
    });
    let listing = listing_of(&newest, name);
    if deleted || content(&newest).is_none() {
        return Err(not_found(
            ErrorCode::ListingWithdrawn,
            format!(
                "The Listing {} has been withdrawn by its author, so it is not for sale.",
                coordinate.text()
            ),
        ));
    }
    let listing = listing.ok_or_else(|| {
        not_found(
            ErrorCode::ListingNotFound,
            format!(
                "The Listing {} has no `version` and `price`.",
                coordinate.text()
            ),
        )
    })?;
    let profile = profiles
        .iter()
        .filter(|event| authentic(event, PROFILE_KIND, author))
        .max_by_key(|event| event["created_at"].as_u64())
        .and_then(profile_of)
        .ok_or_else(|| {
            not_found(
                ErrorCode::ListingNotFound,
                format!(
                    "A Listing whose provider has no Profile (kind {PROFILE_KIND}) with an \
                     `ilp_address`, a `connector_url` and a `connector_seal_key` is not \
                     purchasable: none was found for {author}."
                ),
            )
        })?;
    Ok(Resolved { listing, profile })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_keeps_the_colons_of_its_name() {
        let author = "ab".repeat(32);
        let parsed = parse_coordinate(&format!("30432:{author}:a:b")).unwrap();
        assert_eq!(parsed.name, "a:b");
        assert_eq!(parsed.author, author);
        assert!(parse_coordinate(&format!("30433:{author}:x")).is_err());
        assert!(parse_coordinate(&format!("30432:{}:x", "AB".repeat(32))).is_err());
        assert!(parse_coordinate("basic").is_err());
    }
}
