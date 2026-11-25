//! A paid destination: where a packet is paid for and the key it is sealed to.
//!
//! Four sources name one, each with its own parser: a relay's information document, a tip
//! route event, a provider's Profile and the supervisor's `tip` order reply. The seal-key
//! rule is here once: 65 bytes of hex beginning `04`, with or without `0x`, the shape of
//! the connector's `PublicKeyBytes`.

use serde_json::Value;

use crate::event;
use crate::outcome::Error;

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
        let long = format!("04{}", "ab".repeat(64));
        let wrong_prefix = format!("02{}", "ab".repeat(64));
        let short = format!("04{}", "ab".repeat(63));
        let not_hex = "zz".repeat(65);
        for bad in [compressed, short, wrong_prefix, not_hex, String::new()] {
            assert!(seal_key(&bad).is_none(), "{bad}");
            assert!(relay_document(&with_key(&toon(), &bad)).is_err(), "{bad}");
            let mut event = tip_event();
            event["tags"][2][1] = json!(bad);
            assert!(tip_route(&event).is_err(), "{bad}");
            let mut reply = reply();
            reply["connector_seal_key"] = json!(bad);
            assert!(order_reply(&reply).is_none(), "{bad}");
        }
        assert!(seal_key(&long).is_some());
        // 33 and 64 bytes.
        assert!(seal_key(&"04".repeat(33)).is_none());
        assert!(seal_key(&"04".repeat(64)).is_none());
    }
}
