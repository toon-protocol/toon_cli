//! NIP-44 version 2: the encryption a private message is sealed with.
//!
//! A conversation key comes from secp256k1 ECDH (the x coordinate, unhashed) and an HKDF
//! extract; each message gets 76 bytes of keys from an HKDF expand over a random nonce:
//! a ChaCha20 key and nonce, and an HMAC-SHA256 key. The plaintext is padded to the NIP's
//! lengths, encrypted, and authenticated over the nonce and the ciphertext.
//!
//! This is not the connector's "giftwrap", which seals packets by a different construction.

use base64::Engine;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use chacha20::ChaCha20;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use k256::{NonZeroScalar, PublicKey};
use sha2::Sha256;

use crate::keystore;

const SALT: &[u8] = b"nip44-v2";
const VERSION: u8 = 2;
const MIN_PLAINTEXT: usize = 1;
const MAX_PLAINTEXT: usize = 65535;

/// Why a payload could not be made or opened.
#[derive(Debug, PartialEq, Eq)]
pub struct Nip44Error(pub String);

fn fail<T>(message: &str) -> Result<T, Nip44Error> {
    Err(Nip44Error(message.to_owned()))
}

/// The conversation key of the secret `secret` and the x-only public key `public`: the same
/// for either side of the conversation.
pub fn conversation_key(secret: &[u8; 32], public: &[u8; 32]) -> Result<[u8; 32], Nip44Error> {
    let scalar = NonZeroScalar::try_from(secret.as_slice())
        .ok()
        .ok_or_else(|| Nip44Error("The secret key is not a valid key.".into()))?;
    let mut sec1 = [2u8; 33];
    sec1[1..].copy_from_slice(public);
    let point = PublicKey::from_sec1_bytes(&sec1)
        .map_err(|_| Nip44Error("The public key is not a point on the curve.".into()))?;
    let shared = k256::ecdh::diffie_hellman(scalar, point.as_affine());
    let (prk, _) = Hkdf::<Sha256>::extract(Some(SALT), shared.raw_secret_bytes());
    Ok(prk.into())
}

/// The ChaCha20 key and nonce and the HMAC key of one message.
struct MessageKeys {
    chacha_key: [u8; 32],
    chacha_nonce: [u8; 12],
    hmac_key: [u8; 32],
}

fn message_keys(conversation_key: &[u8; 32], nonce: &[u8; 32]) -> Result<MessageKeys, Nip44Error> {
    let hkdf = Hkdf::<Sha256>::from_prk(conversation_key)
        .map_err(|_| Nip44Error("The conversation key has the wrong length.".into()))?;
    let mut okm = [0u8; 76];
    hkdf.expand(nonce, &mut okm)
        .map_err(|_| Nip44Error("The message keys could not be derived.".into()))?;
    Ok(MessageKeys {
        chacha_key: okm[..32].try_into().expect("32 bytes"),
        chacha_nonce: okm[32..44].try_into().expect("12 bytes"),
        hmac_key: okm[44..].try_into().expect("32 bytes"),
    })
}

/// The length a plaintext of `length` bytes is padded to.
pub fn padded_length(length: usize) -> usize {
    if length <= 32 {
        return 32;
    }
    let next_power = 1usize << (usize::BITS - (length - 1).leading_zeros());
    let chunk = if next_power <= 256 {
        32
    } else {
        next_power / 8
    };
    chunk * ((length - 1) / chunk + 1)
}

fn pad(plaintext: &str) -> Result<Vec<u8>, Nip44Error> {
    let bytes = plaintext.as_bytes();
    if !(MIN_PLAINTEXT..=MAX_PLAINTEXT).contains(&bytes.len()) {
        return fail("A message is 1 to 65535 bytes.");
    }
    let mut padded = Vec::with_capacity(2 + padded_length(bytes.len()));
    padded.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    padded.extend_from_slice(bytes);
    padded.resize(2 + padded_length(bytes.len()), 0);
    Ok(padded)
}

// Opening is built with sealing so that the round trip is tested in one place; the
// supervisor opens wraps in the next ticket.
#[cfg_attr(not(test), allow(dead_code))]
fn unpad(padded: &[u8]) -> Result<String, Nip44Error> {
    let invalid = || Nip44Error("The padding is invalid.".into());
    if padded.len() < 2 {
        return Err(invalid());
    }
    let length = usize::from(u16::from_be_bytes([padded[0], padded[1]]));
    let body = &padded[2..];
    if length == 0 || body.len() != padded_length(length) || length > body.len() {
        return Err(invalid());
    }
    String::from_utf8(body[..length].to_vec()).map_err(|_| invalid())
}

fn mac(hmac_key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(hmac_key).expect("HMAC takes any length");
    mac.update(nonce);
    mac.update(ciphertext);
    mac
}

/// Encrypt `plaintext` under `conversation_key` with a fresh random nonce.
pub fn encrypt(conversation_key: &[u8; 32], plaintext: &str) -> Result<String, Nip44Error> {
    let nonce = keystore::random::<32>().map_err(|error| Nip44Error(error.message))?;
    encrypt_with_nonce(conversation_key, plaintext, &nonce)
}

/// [`encrypt`] with the nonce given, which only a test has reason to do.
pub fn encrypt_with_nonce(
    conversation_key: &[u8; 32],
    plaintext: &str,
    nonce: &[u8; 32],
) -> Result<String, Nip44Error> {
    let keys = message_keys(conversation_key, nonce)?;
    let mut ciphertext = pad(plaintext)?;
    ChaCha20::new(&keys.chacha_key.into(), &keys.chacha_nonce.into())
        .apply_keystream(&mut ciphertext);
    let tag = mac(&keys.hmac_key, nonce, &ciphertext)
        .finalize()
        .into_bytes();
    let mut payload = Vec::with_capacity(1 + 32 + ciphertext.len() + 32);
    payload.push(VERSION);
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&ciphertext);
    payload.extend_from_slice(&tag);
    Ok(base64::engine::general_purpose::STANDARD.encode(payload))
}

// Opening is built with sealing so that the round trip is tested in one place; the
// supervisor opens wraps in the next ticket.
#[cfg_attr(not(test), allow(dead_code))]
/// Open `payload` with `conversation_key`.
pub fn decrypt(conversation_key: &[u8; 32], payload: &str) -> Result<String, Nip44Error> {
    if payload.starts_with('#') {
        return fail("Unknown encryption version.");
    }
    if !(132..=87472).contains(&payload.len()) {
        return fail("The payload has an invalid length.");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| Nip44Error("The payload is not base64.".into()))?;
    if !(99..=65603).contains(&bytes.len()) {
        return fail("The payload has an invalid length.");
    }
    if bytes[0] != VERSION {
        return fail("Unknown encryption version.");
    }
    let (nonce, rest) = bytes[1..].split_at(32);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let nonce: &[u8; 32] = nonce.try_into().expect("32 bytes");
    let keys = message_keys(conversation_key, nonce)?;
    mac(&keys.hmac_key, nonce, ciphertext)
        .verify_slice(tag)
        .map_err(|_| Nip44Error("The message authentication code is invalid.".into()))?;
    let mut padded = ciphertext.to_vec();
    ChaCha20::new(&keys.chacha_key.into(), &keys.chacha_nonce.into()).apply_keystream(&mut padded);
    unpad(&padded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sha2::Digest;

    fn vectors() -> Value {
        let text = include_str!("../tests/vectors/nip44.vectors.json");
        serde_json::from_str::<Value>(text).unwrap()["v2"].clone()
    }

    fn bytes<const N: usize>(value: &Value) -> [u8; N] {
        hex::decode(value.as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap()
    }

    #[test]
    fn conversation_keys_match_the_published_vectors() {
        let valid = vectors()["valid"]["get_conversation_key"].clone();
        for case in valid.as_array().unwrap() {
            let key = conversation_key(&bytes(&case["sec1"]), &bytes(&case["pub2"])).unwrap();
            assert_eq!(key, bytes::<32>(&case["conversation_key"]), "{case}");
        }
    }

    #[test]
    fn conversation_keys_refuse_what_the_published_vectors_say_is_invalid() {
        let invalid = vectors()["invalid"]["get_conversation_key"].clone();
        for case in invalid.as_array().unwrap() {
            assert!(
                conversation_key(&bytes(&case["sec1"]), &bytes(&case["pub2"])).is_err(),
                "{case}"
            );
        }
    }

    #[test]
    fn message_keys_match_the_published_vectors() {
        let set = vectors()["valid"]["get_message_keys"].clone();
        let conversation: [u8; 32] = bytes(&set["conversation_key"]);
        for case in set["keys"].as_array().unwrap() {
            let keys = message_keys(&conversation, &bytes(&case["nonce"])).unwrap();
            assert_eq!(keys.chacha_key, bytes::<32>(&case["chacha_key"]));
            assert_eq!(keys.chacha_nonce, bytes::<12>(&case["chacha_nonce"]));
            assert_eq!(keys.hmac_key, bytes::<32>(&case["hmac_key"]));
        }
    }

    #[test]
    fn padding_matches_the_published_vectors() {
        let cases = vectors()["valid"]["calc_padded_len"].clone();
        for case in cases.as_array().unwrap() {
            let (length, padded) = (case[0].as_u64().unwrap(), case[1].as_u64().unwrap());
            assert_eq!(padded_length(length as usize), padded as usize, "{case}");
        }
    }

    #[test]
    fn encryption_and_decryption_match_the_published_vectors() {
        let cases = vectors()["valid"]["encrypt_decrypt"].clone();
        for case in cases.as_array().unwrap() {
            let conversation: [u8; 32] = bytes(&case["conversation_key"]);
            let plaintext = case["plaintext"].as_str().unwrap();
            let payload = case["payload"].as_str().unwrap();
            assert_eq!(
                encrypt_with_nonce(&conversation, plaintext, &bytes(&case["nonce"])).unwrap(),
                payload
            );
            assert_eq!(decrypt(&conversation, payload).unwrap(), plaintext);
            // The key the vector derives it from is the same from either side.
            let from_secrets = conversation_key(
                &bytes(&case["sec1"]),
                &hex::decode(crate::derive::nostr_public_key(&bytes(&case["sec2"])))
                    .unwrap()
                    .try_into()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(from_secrets, conversation);
        }
    }

    #[test]
    fn long_messages_match_the_published_vectors() {
        let cases = vectors()["valid"]["encrypt_decrypt_long_msg"].clone();
        for case in cases.as_array().unwrap() {
            let conversation: [u8; 32] = bytes(&case["conversation_key"]);
            let plaintext = case["pattern"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap() as usize);
            assert_eq!(
                hex::encode(Sha256::digest(plaintext.as_bytes())),
                case["plaintext_sha256"].as_str().unwrap()
            );
            let payload =
                encrypt_with_nonce(&conversation, &plaintext, &bytes(&case["nonce"])).unwrap();
            assert_eq!(
                hex::encode(Sha256::digest(payload.as_bytes())),
                case["payload_sha256"].as_str().unwrap()
            );
            assert_eq!(decrypt(&conversation, &payload).unwrap(), plaintext);
        }
    }

    #[test]
    fn a_message_of_a_length_the_nip_forbids_is_not_encrypted() {
        let conversation = [9u8; 32];
        let lengths = vectors()["invalid"]["encrypt_msg_lengths"].clone();
        for length in lengths.as_array().unwrap() {
            let length = length.as_u64().unwrap() as usize;
            assert!(
                encrypt(&conversation, &"x".repeat(length)).is_err(),
                "{length}"
            );
        }
        assert!(encrypt(&conversation, &"x".repeat(65535)).is_ok());
    }

    #[test]
    fn payloads_the_published_vectors_say_are_invalid_are_refused() {
        let cases = vectors()["invalid"]["decrypt"].clone();
        for case in cases.as_array().unwrap() {
            let conversation: [u8; 32] = bytes(&case["conversation_key"]);
            assert!(
                decrypt(&conversation, case["payload"].as_str().unwrap()).is_err(),
                "{case}"
            );
        }
    }
}
