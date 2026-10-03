//! NIP-59: a rumor sealed and gift wrapped.
//!
//! A rumor is an event nobody signed. It is sealed in a kind 13 event signed by its sender,
//! with no tags and the rumor, encrypted to the recipient, as content. The seal is wrapped
//! in a kind 1059 event signed by a key made for that wrap alone, with one `p` tag naming
//! the recipient. The seal's and the wrap's `created_at` are random, up to two days back,
//! so that their times say nothing; the rumor keeps the real one.

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde_json::{json, Value};

use crate::event;
use crate::keystore;
use crate::nip44::{self, Nip44Error};

pub const SEAL: u64 = 13;
pub const WRAP: u64 = 1059;

/// How far back the seal's and the wrap's `created_at` may be set, in seconds.
const TWO_DAYS: u64 = 2 * 24 * 60 * 60;

/// Why a wrap could not be made or opened.
#[derive(Debug, PartialEq, Eq)]
pub struct WrapError(pub String);

impl From<Nip44Error> for WrapError {
    fn from(error: Nip44Error) -> Self {
        WrapError(error.0)
    }
}

impl From<crate::outcome::Error> for WrapError {
    fn from(error: crate::outcome::Error) -> Self {
        WrapError(error.message)
    }
}

fn refuse<T>(message: &str) -> Result<T, WrapError> {
    Err(WrapError(message.to_owned()))
}

/// An unsigned event of `kind`: the event, as NIP-01 forms it, without a `sig`.
pub fn rumor(pubkey: &str, created_at: u64, kind: u64, tags: Value, content: &str) -> Value {
    let id = event::event_id(pubkey, created_at, kind, &tags, content);
    json!({
        "id": hex::encode(id),
        "pubkey": pubkey,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
    })
}

/// A time up to two days before `now`.
fn blurred(now: u64) -> Result<u64, WrapError> {
    let random = u64::from_be_bytes(keystore::random::<8>()?);
    Ok(now.saturating_sub(random % TWO_DAYS))
}

fn x_only(hex_key: &str) -> Result<[u8; 32], WrapError> {
    hex::decode(hex_key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| WrapError(format!("{hex_key} is not a public key.")))
}

/// A secret key made for one wrap, which nothing else keeps.
fn one_time_secret() -> Result<[u8; 32], WrapError> {
    loop {
        let secret = keystore::random::<32>()?;
        if SigningKey::from_bytes(&secret).is_ok() {
            return Ok(secret);
        }
    }
}

/// The rumor sealed by `sender` for `recipient` (an x-only key in hex), and wrapped. The
/// seal and the wrap are made `now`, less a random time.
pub fn wrap(
    rumor: &Value,
    sender: &[u8; 32],
    recipient: &str,
    now: u64,
) -> Result<Value, WrapError> {
    let recipient_key = x_only(recipient)?;
    let sealed = nip44::encrypt(
        &nip44::conversation_key(sender, &recipient_key)?,
        &rumor.to_string(),
    )?;
    let seal = event::sign(sender, blurred(now)?, SEAL, json!([]), &sealed)?;

    let one_time = one_time_secret()?;
    let wrapped = nip44::encrypt(
        &nip44::conversation_key(&one_time, &recipient_key)?,
        &seal.to_string(),
    )?;
    Ok(event::sign(
        &one_time,
        blurred(now)?,
        WRAP,
        json!([["p", recipient]]),
        &wrapped,
    )?)
}

/// Whether `event` carries the id and the signature NIP-01 defines.
fn is_signed(event: &Value) -> bool {
    let text = |field: &str| event[field].as_str().map(str::to_owned);
    let (Some(id), Some(pubkey), Some(content), Some(sig)) =
        (text("id"), text("pubkey"), text("content"), text("sig"))
    else {
        return false;
    };
    let (Some(created_at), Some(kind)) = (event["created_at"].as_u64(), event["kind"].as_u64())
    else {
        return false;
    };
    let computed = event::event_id(&pubkey, created_at, kind, &event["tags"], &content);
    if hex::encode(computed) != id {
        return false;
    }
    let (Ok(key), Ok(signature)) = (
        hex::decode(&pubkey)
            .map_err(drop)
            .and_then(|bytes| {
                if bytes.len() == 32 {
                    Ok(bytes)
                } else {
                    Err(())
                }
            })
            .and_then(|bytes| VerifyingKey::from_bytes(&bytes).map_err(drop)),
        hex::decode(&sig)
            .map_err(drop)
            .and_then(|bytes| Signature::try_from(bytes.as_slice()).map_err(drop)),
    ) else {
        return false;
    };
    key.verify_raw(&computed, &signature).is_ok()
}

/// What opening a wrap yields: the rumor, and the key that sealed it.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened {
    pub rumor: Value,
    pub sender: String,
}

/// Open the wrap `wrap` with the secret of the key it was sent to. A wrap or a seal that is
/// not signed as it says, a seal that is not a kind 13 with no tags, and a seal whose
/// `pubkey` is not the rumor's, are refused.
pub fn open(wrap: &Value, recipient: &[u8; 32]) -> Result<Opened, WrapError> {
    if wrap["kind"] != WRAP || !is_signed(wrap) {
        return refuse("This is not a signed gift wrap.");
    }
    let content = |event: &Value| event["content"].as_str().unwrap_or_default().to_owned();

    let wrapper = x_only(wrap["pubkey"].as_str().unwrap_or_default())?;
    let seal_text = nip44::decrypt(
        &nip44::conversation_key(recipient, &wrapper)?,
        &content(wrap),
    )?;
    let seal: Value =
        serde_json::from_str(&seal_text).map_err(|_| WrapError("The seal is not JSON.".into()))?;
    if seal["kind"] != SEAL || !is_signed(&seal) {
        return refuse("The wrap holds no signed seal.");
    }
    if seal["tags"].as_array().is_none_or(|tags| !tags.is_empty()) {
        return refuse("The seal has tags.");
    }

    let sender = seal["pubkey"].as_str().unwrap_or_default().to_owned();
    let rumor_text = nip44::decrypt(
        &nip44::conversation_key(recipient, &x_only(&sender)?)?,
        &content(&seal),
    )?;
    let rumor: Value = serde_json::from_str(&rumor_text)
        .map_err(|_| WrapError("The rumor is not JSON.".into()))?;
    if rumor["pubkey"] != seal["pubkey"] {
        return refuse("The seal was signed by a key other than the rumor's.");
    }
    if rumor.get("sig").is_some_and(|sig| !sig.is_null()) {
        return refuse("The rumor is signed.");
    }
    let id = event::event_id(
        &sender,
        rumor["created_at"].as_u64().unwrap_or(0),
        rumor["kind"].as_u64().unwrap_or(0),
        &rumor["tags"],
        rumor["content"].as_str().unwrap_or_default(),
    );
    if rumor["id"] != hex::encode(id) {
        return refuse("The rumor's id is not the one its fields make.");
    }
    Ok(Opened { rumor, sender })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> ([u8; 32], String) {
        let secret = [byte; 32];
        (secret, event::public_key(&secret).unwrap())
    }

    fn a_rumor(sender: &str) -> Value {
        rumor(sender, 1_700_000_000, 14, json!([["p", "ab"]]), "hello")
    }

    #[test]
    fn a_wrap_opens_to_the_rumor_it_was_made_from() {
        let (sender, sender_key) = key(3);
        let (recipient, recipient_key) = key(5);
        let rumor = a_rumor(&sender_key);

        let wrapped = wrap(&rumor, &sender, &recipient_key, 1_700_000_000).unwrap();

        assert_eq!(wrapped["kind"], WRAP);
        assert_eq!(wrapped["tags"], json!([["p", recipient_key]]));
        assert_ne!(wrapped["pubkey"], json!(sender_key));
        assert!(!wrapped["content"].as_str().unwrap().contains("hello"));
        let opened = open(&wrapped, &recipient).unwrap();
        assert_eq!(opened.rumor, rumor);
        assert_eq!(opened.sender, sender_key);
    }

    #[test]
    fn the_times_of_the_seal_and_the_wrap_are_blurred_and_never_in_the_future() {
        let (sender, sender_key) = key(3);
        let (_, recipient_key) = key(5);
        let now = 1_700_000_000;
        let rumor = a_rumor(&sender_key);
        for _ in 0..20 {
            let wrapped = wrap(&rumor, &sender, &recipient_key, now).unwrap();
            let at = wrapped["created_at"].as_u64().unwrap();
            assert!(at <= now && at > now - TWO_DAYS, "{at}");
        }
    }

    #[test]
    fn another_secret_does_not_open_a_wrap() {
        let (sender, sender_key) = key(3);
        let (_, recipient_key) = key(5);
        let (other, _) = key(6);
        let wrapped = wrap(&a_rumor(&sender_key), &sender, &recipient_key, 1).unwrap();

        assert!(open(&wrapped, &other).is_err());
    }

    #[test]
    fn a_seal_signed_by_a_key_other_than_the_rumors_is_refused() {
        let (sender, _) = key(3);
        let (_, other_key) = key(4);
        let (recipient, recipient_key) = key(5);
        // The rumor claims to be from `other`, but `sender` seals it.
        let forged = a_rumor(&other_key);

        let wrapped = wrap(&forged, &sender, &recipient_key, 1).unwrap();

        let refused = open(&wrapped, &recipient).unwrap_err();
        assert!(refused.0.contains("other than the rumor's"), "{refused:?}");
    }

    #[test]
    fn an_event_with_a_key_of_the_wrong_length_is_refused_and_does_not_panic() {
        let (recipient, _) = key(5);
        let mut wrapped = json!({
            "id": "00", "pubkey": "abcd", "created_at": 1, "kind": WRAP, "tags": [],
            "content": "x", "sig": "00",
        });
        assert!(open(&wrapped, &recipient).is_err());
        wrapped["pubkey"] = json!("ab".repeat(33));
        assert!(open(&wrapped, &recipient).is_err());
    }

    #[test]
    fn a_wrap_altered_after_it_was_signed_is_refused() {
        let (sender, sender_key) = key(3);
        let (recipient, recipient_key) = key(5);
        let mut wrapped = wrap(&a_rumor(&sender_key), &sender, &recipient_key, 1).unwrap();
        wrapped["created_at"] = json!(2);

        assert!(open(&wrapped, &recipient).is_err());
    }
}
