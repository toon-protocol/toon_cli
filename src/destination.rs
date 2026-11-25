//! A paid destination: where a packet is paid for and the key it is sealed to.
//!
//! Four sources name one, each with its own parser: a relay's information document, a tip
//! route event, a provider's Profile and the supervisor's `tip` order reply. The seal-key
//! rule is here once: 65 bytes of hex beginning `04`, with or without `0x`, the shape of
//! the connector's `PublicKeyBytes`.
//!
//! So is the preflight every command runs before it pays a destination: the connector's
//! served identity matches the pinned seal key, where the command talks to that connector
//! anyway, and a route of this agent node forwards the ILP address.

use std::path::Path;

use serde_json::Value;

use crate::egress::Egress;
use crate::event;
use crate::operator;
use crate::outcome::{Error, ErrorCode};

/// Where a packet is paid for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub ilp_address: String,
    /// A location hint: where to peer, never dialled by a write. Empty when the source
    /// does not name one (the supervisor's `tip` order reply).
    pub connector_url: String,
    /// The connector's sealing public key, which a packet is sealed to.
    pub seal_key: [u8; 65],
}

/// The `connector_seal_key`: 65 bytes of hex beginning `04`, with or without `0x`.
pub fn seal_key(text: &str) -> Option<[u8; 65]> {
    hex::decode(text.strip_prefix("0x").unwrap_or(text))
        .ok()
        .and_then(|key| key.try_into().ok())
        .filter(|key: &[u8; 65]| key[0] == 0x04)
}

fn key_of(text: &str) -> Result<[u8; 65], String> {
    seal_key(text).ok_or_else(|| {
        format!("a `connector_seal_key` that is not 65 bytes of hex beginning `04`: {text}")
    })
}

fn present(value: Option<&str>, field: &str) -> Result<String, String> {
    value
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("no `{field}`"))
}

/// The destination in the `toon` object of a relay's information document and the price
/// of a write there, or what the object is missing to be one.
pub fn relay_document(toon: &Value) -> Result<(Destination, u64), String> {
    let text = |field: &str| present(toon[field].as_str(), field);
    let destination = Destination {
        seal_key: key_of(&text("connector_seal_key")?)?,
        ilp_address: text("ilp_address")?,
        connector_url: text("connector_url")?,
    };
    let price = toon["price"]
        .as_u64()
        .ok_or_else(|| "no `price`".to_owned())?;
    Ok((destination, price))
}

/// The refusal for an information document of `relay` whose `toon` object is not a
/// destination, `missing` saying what it lacks.
pub fn unpayable_document(relay: &str, missing: &str) -> Error {
    event::unpayable(format!(
        "The information document of {relay} does not say where it is paid: its `toon` \
         object needs an `ilp_address`, a `connector_url`, a `connector_seal_key` of 65 \
         bytes of hex beginning `04` and a `price`, and has {missing}."
    ))
}

/// The destination a tip route event (kind 10850) names in its tags.
pub fn tip_route(event: &Value) -> Result<Destination, String> {
    let named = |name: &str| {
        let value = event["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_array())
            .find(|tag| tag.first().and_then(Value::as_str) == Some(name))
            .and_then(|tag| tag.get(1))
            .and_then(Value::as_str);
        present(value, name)
    };
    Ok(Destination {
        seal_key: key_of(&named("connector_seal_key")?)?,
        connector_url: named("connector_url")?,
        ilp_address: named("ilp_address")?,
    })
}

/// The destination in the content of a provider's Profile event, if it is one.
pub fn profile(event: &Value) -> Option<Destination> {
    let content = serde_json::from_str::<Value>(event["content"].as_str()?).ok()?;
    let text = |field: &str| present(content[field].as_str(), field).ok();
    Some(Destination {
        ilp_address: text("ilp_address")?,
        connector_url: text("connector_url")?,
        seal_key: seal_key(&text("connector_seal_key")?)?,
    })
}

/// The destination in the supervisor's reply to a `tip` order, which names no connector
/// URL.
pub fn order_reply(reply: &Value) -> Option<Destination> {
    Some(Destination {
        ilp_address: present(reply["ilp_address"].as_str(), "ilp_address").ok()?,
        connector_url: String::new(),
        seal_key: seal_key(reply["connector_seal_key"].as_str()?)?,
    })
}

/// Where a packet to a destination leaves, and so where the preflight looks for its route.
pub enum Leaving<'a> {
    /// Through the connector `--app` names, in this home.
    Home(&'a Path),
    /// Through the connector of the named TOON app, in this home: the supervisor paying for
    /// an agent key's order.
    App(&'a Path, &'a str),
    /// Sent with an agent key: there is no home to look in, and its connector answers for
    /// the route.
    AgentKey,
}

/// The identity check of a command that talks to the destination's connector anyway (tip,
/// compute): its served `/identity`, asked through `egress`, must be the pinned seal key, or
/// the command fails with `mismatch`. A write edge is never identity-checked: a write to
/// another relay, a private message and a subscription make no request to its
/// `connector_url`.
pub struct Pinned<'a> {
    pub egress: &'a Egress,
    pub mismatch: Error,
}

/// What the preflight found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Good,
    /// The connector serves another seal key than the pinned one.
    Mismatch,
    /// No route of this agent node forwards the ILP address.
    NoRoute,
}

/// The preflight of a command about to pay `destination`. The identity check, when the
/// command asks for one, comes first; then a route of this agent node must forward the ILP
/// address, else `peering_needed`, naming `deposit` when the command knows what the peering
/// would take. Nothing has been sent when it fails.
pub fn preflight(
    destination: &Destination,
    leaving: Leaving,
    pinned: Option<Pinned>,
    deposit: Option<u128>,
) -> Result<(), Error> {
    let served = match &pinned {
        Some(pinned) => Some(operator::identity(
            pinned.egress,
            &destination.connector_url,
        )?),
        None => None,
    };
    let found = verdict(destination, served, || {
        Ok(match leaving {
            Leaving::Home(home) => Some(operator::forwards(home, &destination.ilp_address)?),
            Leaving::App(home, toon_app) => Some(operator::forwards_through(
                home,
                toon_app,
                &destination.ilp_address,
            )?),
            Leaving::AgentKey => None,
        })
    })?;
    match (found, leaving) {
        (Verdict::Good, _) => Ok(()),
        (Verdict::Mismatch, _) => Err(pinned.expect("a mismatch was checked").mismatch),
        (Verdict::NoRoute, Leaving::Home(home) | Leaving::App(home, _)) => {
            Err(peering_needed(home, destination, deposit))
        }
        (Verdict::NoRoute, Leaving::AgentKey) => unreachable!("an agent key looks for no route"),
    }
}

/// The preflight's answer from the key the connector served (`None` when it was not asked)
/// and whether a route forwards the ILP address (`None` with an agent key, whose connector
/// answers for it). The route is looked for only once the key matches.
fn verdict(
    destination: &Destination,
    served: Option<[u8; 65]>,
    forwards: impl FnOnce() -> Result<Option<bool>, Error>,
) -> Result<Verdict, Error> {
    if served.is_some_and(|served| served != destination.seal_key) {
        return Ok(Verdict::Mismatch);
    }
    Ok(match forwards()? {
        Some(false) => Verdict::NoRoute,
        Some(true) | None => Verdict::Good,
    })
}

/// The `peering_needed` error for `destination`, which no route of this agent node forwards.
/// A peering that cannot be looked up is not recognised, so the lookup never changes the
/// error.
fn peering_needed(home: &Path, destination: &Destination, deposit: Option<u128>) -> Error {
    let reaching = operator::peering_toward(home, &destination.connector_url)
        .ok()
        .flatten();
    Error {
        code: ErrorCode::PeeringNeeded,
        message: peering_message(
            &destination.ilp_address,
            &destination.connector_url,
            deposit,
            reaching.as_deref(),
        ),
        nothing_sent: false,
        unanswered: None,
    }
}

/// What to run so that a packet to `address`, paid at the connector at `connector_url`, is
/// forwarded: only a route over `reaching`, the peering that already reaches that connector,
/// else a peering with `deposit` (when known) and a route over it. The one wording of
/// `peering_needed`.
pub fn peering_message(
    address: &str,
    connector_url: &str,
    deposit: Option<u128>,
    reaching: Option<&str>,
) -> String {
    if let Some(id) = reaching {
        return format!(
            "No route of this agent node forwards {address}, but its peering {id} already \
             reaches {connector_url}: no new deposit is needed. Run \
             `toon route add {address} --peer {id}`."
        );
    }
    let (needed, amount) = match deposit {
        Some(deposit) => (
            format!(", with a deposit of at least {deposit} base units"),
            deposit.to_string(),
        ),
        None => (String::new(), "<amount>".to_owned()),
    };
    format!(
        "No peering of this agent node reaches {address}. A peering is needed{needed}: run \
         `{}` and then `toon route add {address} --peer <id>`.",
        operator::peer_add_command(connector_url, &amount, "")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn key() -> String {
        format!("04{}", "ab".repeat(64))
    }

    fn toon() -> Value {
        json!({
            "ilp_address": "g.far",
            "connector_url": "http://far",
            "connector_seal_key": key(),
            "price": 7,
        })
    }

    fn tip_event() -> Value {
        json!({ "tags": [
            ["ilp_address", "g.far"],
            ["connector_url", "http://far"],
            ["connector_seal_key", key()],
        ] })
    }

    fn profile_event() -> Value {
        json!({ "content": json!({
            "ilp_address": "g.far",
            "connector_url": "http://far",
            "connector_seal_key": key(),
        }).to_string() })
    }

    fn reply() -> Value {
        json!({ "ilp_address": "g.far", "connector_seal_key": key() })
    }

    fn without(value: &Value, field: &str) -> Value {
        let mut value = value.clone();
        value.as_object_mut().unwrap().remove(field);
        value
    }

    fn without_tag(event: &Value, name: &str) -> Value {
        let tags: Vec<Value> = event["tags"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tag| tag[0] != name)
            .cloned()
            .collect();
        json!({ "tags": tags })
    }

    fn with_key(toon: &Value, key: &str) -> Value {
        let mut toon = toon.clone();
        toon["connector_seal_key"] = json!(key);
        toon
    }

    #[test]
    fn a_relay_document_names_a_destination_and_a_price() {
        let (destination, price) = relay_document(&toon()).unwrap();
        assert_eq!(destination.ilp_address, "g.far");
        assert_eq!(destination.connector_url, "http://far");
        assert_eq!(destination.seal_key[0], 0x04);
        assert_eq!(price, 7);
    }

    #[test]
    fn a_relay_document_missing_a_field_says_which() {
        for field in [
            "ilp_address",
            "connector_url",
            "connector_seal_key",
            "price",
        ] {
            let missing = relay_document(&without(&toon(), field)).unwrap_err();
            assert!(missing.contains(field), "{missing}");
        }
        let mut blank = toon();
        blank["ilp_address"] = json!("");
        let empty = relay_document(&blank).unwrap_err();
        assert!(empty.contains("ilp_address"), "{empty}");
        assert!(relay_document(&Value::Null).is_err());
    }

    #[test]
    fn a_tip_route_names_a_destination() {
        let route = tip_route(&tip_event()).unwrap();
        assert_eq!(route.ilp_address, "g.far");
        assert_eq!(route.connector_url, "http://far");
        assert_eq!(route.seal_key, seal_key(&key()).unwrap());
    }

    #[test]
    fn a_tip_route_missing_a_tag_says_which() {
        for name in ["ilp_address", "connector_url", "connector_seal_key"] {
            let missing = tip_route(&without_tag(&tip_event(), name)).unwrap_err();
            assert!(missing.contains(name), "{missing}");
        }
    }

    #[test]
    fn a_profile_names_a_destination() {
        let profile = profile(&profile_event()).unwrap();
        assert_eq!(profile.ilp_address, "g.far");
        assert_eq!(profile.connector_url, "http://far");
        assert_eq!(profile.seal_key, seal_key(&key()).unwrap());
    }

    #[test]
    fn a_profile_missing_a_field_is_not_a_destination() {
        let content: Value =
            serde_json::from_str(profile_event()["content"].as_str().unwrap()).unwrap();
        for field in ["ilp_address", "connector_url", "connector_seal_key"] {
            let event = json!({ "content": without(&content, field).to_string() });
            assert!(profile(&event).is_none(), "{field}");
        }
        assert!(profile(&json!({ "content": "not json" })).is_none());
        assert!(profile(&json!({})).is_none());
    }

    #[test]
    fn an_order_reply_names_a_destination_without_a_connector_url() {
        let destination = order_reply(&reply()).unwrap();
        assert_eq!(destination.ilp_address, "g.far");
        assert_eq!(destination.connector_url, "");
        assert_eq!(destination.seal_key, seal_key(&key()).unwrap());
    }

    #[test]
    fn an_order_reply_missing_a_field_is_not_a_destination() {
        for field in ["ilp_address", "connector_seal_key"] {
            assert!(order_reply(&without(&reply(), field)).is_none(), "{field}");
        }
    }

    #[test]
    fn a_seal_key_beginning_04_is_accepted_with_or_without_0x() {
        assert!(seal_key(&key()).is_some());
        assert_eq!(seal_key(&key()), seal_key(&format!("0x{}", key())));
    }

    #[test]
    fn a_key_that_is_not_65_bytes_beginning_04_is_refused_by_every_parser() {
        let compressed = format!("02{}", "ab".repeat(32));
        let wrong_prefix = format!("02{}", "ab".repeat(64));
        let bytes_33 = "04".repeat(33);
        let bytes_64 = "04".repeat(64);
        let not_hex = "zz".repeat(65);
        for bad in [
            compressed,
            wrong_prefix,
            bytes_33,
            bytes_64,
            not_hex,
            String::new(),
        ] {
            assert!(seal_key(&bad).is_none(), "{bad}");
            assert!(relay_document(&with_key(&toon(), &bad)).is_err(), "{bad}");
            let mut event = tip_event();
            event["tags"][2][1] = json!(bad);
            assert!(tip_route(&event).is_err(), "{bad}");
            let content: Value =
                serde_json::from_str(profile_event()["content"].as_str().unwrap()).unwrap();
            let event = json!({ "content": with_key(&content, &bad).to_string() });
            assert!(profile(&event).is_none(), "{bad}");
            let mut reply = reply();
            reply["connector_seal_key"] = json!(bad);
            assert!(order_reply(&reply).is_none(), "{bad}");
        }
    }

    fn destination() -> Destination {
        relay_document(&toon()).unwrap().0
    }

    fn other_key() -> [u8; 65] {
        seal_key(&format!("04{}", "cd".repeat(64))).unwrap()
    }

    #[test]
    fn the_preflight_refuses_a_served_key_other_than_the_pinned_one_before_any_route() {
        let found = verdict(&destination(), Some(other_key()), || {
            panic!("the route is not looked for after a mismatch")
        });
        assert_eq!(found.unwrap(), Verdict::Mismatch);
    }

    #[test]
    fn the_preflight_refuses_a_destination_no_route_forwards() {
        let served = destination().seal_key;
        assert_eq!(
            verdict(&destination(), Some(served), || Ok(Some(false))).unwrap(),
            Verdict::NoRoute
        );
        assert_eq!(
            verdict(&destination(), None, || Ok(Some(false))).unwrap(),
            Verdict::NoRoute
        );
    }

    #[test]
    fn the_preflight_passes_a_matching_key_and_a_forwarding_route() {
        let served = destination().seal_key;
        assert_eq!(
            verdict(&destination(), Some(served), || Ok(Some(true))).unwrap(),
            Verdict::Good
        );
        assert_eq!(
            verdict(&destination(), None, || Ok(Some(true))).unwrap(),
            Verdict::Good
        );
    }

    #[test]
    fn with_an_agent_key_the_connector_answers_for_the_route() {
        assert_eq!(
            verdict(&destination(), None, || Ok(None)).unwrap(),
            Verdict::Good
        );
    }

    #[test]
    fn without_a_peering_the_message_names_the_peer_add_and_the_route_add() {
        assert_eq!(
            peering_message("g.far", "http://far", None, None),
            "No peering of this agent node reaches g.far. A peering is needed: run \
             `toon peer add http://far --deposit <amount> --yes` and then \
             `toon route add g.far --peer <id>`."
        );
    }

    #[test]
    fn without_a_peering_the_message_names_the_deposit_a_command_knows() {
        assert_eq!(
            peering_message("g.far", "http://far", Some(2000), None),
            "No peering of this agent node reaches g.far. A peering is needed, with a \
             deposit of at least 2000 base units: run \
             `toon peer add http://far --deposit 2000 --yes` and then \
             `toon route add g.far --peer <id>`."
        );
    }

    #[test]
    fn with_a_peering_toward_the_connector_the_message_asks_only_for_a_route() {
        assert_eq!(
            peering_message("g.far", "http://far", Some(2000), Some("far")),
            "No route of this agent node forwards g.far, but its peering far already \
             reaches http://far: no new deposit is needed. Run \
             `toon route add g.far --peer far`."
        );
    }
}
