//! Key derivation: one mnemonic, every key an agent node holds.
//!
//! Settlement keys follow `toon-client`'s derivation, so the same mnemonic recovers the
//! same payment addresses in either tool. The key indexed by `connector` is the one
//! `toon-client` calls `accountIndex`:
//!
//! - EVM: `m/44'/60'/0'/0/{connector}`, BIP-32 over secp256k1.
//! - Solana: `m/44'/501'/{connector}'/0'`, SLIP-0010 over Ed25519, all hardened.
//!
//! The other keys sit in a tree of the wallet's own, `m/10473'/...`, all hardened and
//! outside every registered coin type, so none of them can equal a key that pays or
//! settles (ADR 0004), nor one `toon-client` derives:
//!
//! - agent identity: `m/10473'/0'/0'`, once per wallet.
//! - operator write key: `m/10473'/1'/0'`, once per wallet.
//! - connector identity key: `m/10473'/2'/{connector}'`, per connector.
//!
//! Each of those is a secp256k1 key whose public half is a Nostr x-only key, except
//! the operator write key: the connector verifies operator writes with Ed25519, so its
//! 32 bytes are read as an Ed25519 secret and its public half is that key, in hex.

use hmac::{Hmac, Mac};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{ProjectivePoint, Scalar};
use sha2::Sha512;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

use k256::elliptic_curve::PrimeField;

const HARDENED: u32 = 0x8000_0000;
/// The first level of the wallet's own tree.
const TOON_PURPOSE: u32 = 10473;

type HmacSha512 = Hmac<Sha512>;

/// A key that failed to derive: a BIP-32 child that falls outside the curve (a 1 in
/// 2^127 event) or a connector index BIP-32 cannot represent.
#[derive(Debug, PartialEq, Eq)]
pub struct DeriveError(pub String);

/// The largest connector index: BIP-32 indexes are below 2^31.
pub const MAX_CONNECTOR_INDEX: u32 = HARDENED - 1;

/// A secp256k1 key and the chain code that extends it.
struct Node {
    key: Zeroizing<[u8; 32]>,
    chain_code: [u8; 32],
}

fn hmac_sha512(key: &[u8], data: &[u8]) -> [u8; 64] {
    let mut mac = HmacSha512::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn split(output: [u8; 64]) -> Node {
    let mut key = [0u8; 32];
    key.copy_from_slice(&output[..32]);
    let mut chain_code = [0u8; 32];
    chain_code.copy_from_slice(&output[32..]);
    Node {
        key: Zeroizing::new(key),
        chain_code,
    }
}

fn scalar(bytes: &[u8; 32]) -> Option<Scalar> {
    Option::from(Scalar::from_repr((*bytes).into()))
}

fn public_key(secret: &[u8; 32]) -> [u8; 33] {
    let scalar = scalar(secret).expect("a derived key is a valid scalar");
    let point = (ProjectivePoint::GENERATOR * scalar).to_affine();
    let encoded = point.to_encoded_point(true);
    let mut out = [0u8; 33];
    out.copy_from_slice(encoded.as_bytes());
    out
}

fn master(seed: &[u8]) -> Node {
    split(hmac_sha512(b"Bitcoin seed", seed))
}

/// BIP-32 `CKDpriv`.
fn child(parent: &Node, index: u32) -> Result<Node, DeriveError> {
    let mut data = Vec::with_capacity(37);
    if index >= HARDENED {
        data.push(0);
        data.extend_from_slice(parent.key.as_slice());
    } else {
        data.extend_from_slice(&public_key(&parent.key));
    }
    data.extend_from_slice(&index.to_be_bytes());
    let node = split(hmac_sha512(&parent.chain_code, &data));
    let invalid = || DeriveError(format!("BIP-32 index {index} gives no valid key"));
    let tweak = scalar(&node.key).ok_or_else(invalid)?;
    let sum = tweak + scalar(&parent.key).expect("a derived key is a valid scalar");
    if bool::from(sum.is_zero()) {
        return Err(invalid());
    }
    Ok(Node {
        key: Zeroizing::new(sum.to_repr().into()),
        chain_code: node.chain_code,
    })
}

fn secp256k1_path(seed: &[u8], path: &[u32]) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    let mut node = master(seed);
    for index in path {
        node = child(&node, *index)?;
    }
    Ok(node.key)
}

/// SLIP-0010 for Ed25519: every index hardened, so `path` holds the indexes without the
/// hardened bit.
fn ed25519_path(seed: &[u8], path: &[u32]) -> Zeroizing<[u8; 32]> {
    let mut node = split(hmac_sha512(b"ed25519 seed", seed));
    for index in path {
        let mut data = vec![0];
        data.extend_from_slice(node.key.as_slice());
        data.extend_from_slice(&(index | HARDENED).to_be_bytes());
        node = split(hmac_sha512(&node.chain_code, &data));
    }
    node.key
}

fn check_connector(connector: u32) -> Result<(), DeriveError> {
    if connector > MAX_CONNECTOR_INDEX {
        return Err(DeriveError(format!(
            "connector index {connector} is above {MAX_CONNECTOR_INDEX}"
        )));
    }
    Ok(())
}

/// The EVM address (EIP-55) of a secp256k1 secret.
fn evm_address(secret: &[u8; 32]) -> String {
    let scalar = scalar(secret).expect("a derived key is a valid scalar");
    let point = (ProjectivePoint::GENERATOR * scalar).to_affine();
    let uncompressed = point.to_encoded_point(false);
    let hash = Keccak256::digest(&uncompressed.as_bytes()[1..]);
    let lower = hex::encode(&hash[12..]);
    let checksum = Keccak256::digest(lower.as_bytes());
    let mut address = String::from("0x");
    for (position, character) in lower.chars().enumerate() {
        let nibble = (checksum[position / 2] >> (4 * (1 - position % 2))) & 0xf;
        if character.is_ascii_alphabetic() && nibble >= 8 {
            address.push(character.to_ascii_uppercase());
        } else {
            address.push(character);
        }
    }
    address
}

/// The Nostr public key (x-only, hex) of a secp256k1 secret.
fn nostr_public_key(secret: &[u8; 32]) -> String {
    hex::encode(&public_key(secret)[1..])
}

/// What one connector's keys give out.
#[derive(Debug, PartialEq, Eq)]
pub struct ConnectorKeys {
    pub index: u32,
    /// The EVM settlement address.
    pub evm: String,
    /// The Solana settlement address.
    pub solana: String,
    /// The connector's identity key, as a Nostr public key in hex.
    pub identity: String,
}

/// The public halves of every key a wallet derives for connectors `0..connectors`.
#[derive(Debug, PartialEq, Eq)]
pub struct Addresses {
    pub connectors: Vec<ConnectorKeys>,
    /// The operator write key, as an Ed25519 public key in hex: what a connector's
    /// `write_keys_file` lists.
    pub operator_write: String,
    /// The agent identity, as a Nostr public key in hex.
    pub agent_identity: String,
}

/// The seed a mnemonic gives, with no passphrase, as BIP-39 defines it.
pub fn seed(mnemonic: &bip39::Mnemonic) -> Zeroizing<[u8; 64]> {
    Zeroizing::new(mnemonic.to_seed(""))
}

pub fn evm_settlement_secret(
    seed: &[u8],
    connector: u32,
) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    check_connector(connector)?;
    secp256k1_path(
        seed,
        &[44 | HARDENED, 60 | HARDENED, HARDENED, 0, connector],
    )
}

pub fn solana_settlement_secret(
    seed: &[u8],
    connector: u32,
) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    check_connector(connector)?;
    Ok(ed25519_path(seed, &[44, 501, connector, 0]))
}

pub fn identity_secret(seed: &[u8], connector: u32) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    check_connector(connector)?;
    secp256k1_path(
        seed,
        &[TOON_PURPOSE | HARDENED, 2 | HARDENED, connector | HARDENED],
    )
}

pub fn operator_write_secret(seed: &[u8]) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    secp256k1_path(seed, &[TOON_PURPOSE | HARDENED, 1 | HARDENED, HARDENED])
}

pub fn agent_identity_secret(seed: &[u8]) -> Result<Zeroizing<[u8; 32]>, DeriveError> {
    secp256k1_path(seed, &[TOON_PURPOSE | HARDENED, HARDENED, HARDENED])
}

/// The public half of the operator write key, as a connector's write-key allowlist
/// lists it: the Ed25519 public key in hex.
pub fn operator_write_public_key(secret: &[u8; 32]) -> String {
    hex::encode(
        ed25519_dalek::SigningKey::from_bytes(secret)
            .verifying_key()
            .as_bytes(),
    )
}

/// The Solana address of a SLIP-0010 seed: base58 of the Ed25519 public key.
fn solana_address(secret: &[u8; 32]) -> String {
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    bs58::encode(signing.verifying_key().as_bytes()).into_string()
}

/// Every public key for connectors `0..connectors`, plus the once-per-wallet keys.
pub fn addresses(seed: &[u8], connectors: u32) -> Result<Addresses, DeriveError> {
    let mut keys = Vec::new();
    for index in 0..connectors {
        keys.push(ConnectorKeys {
            index,
            evm: evm_address(&*evm_settlement_secret(seed, index)?),
            solana: solana_address(&*solana_settlement_secret(seed, index)?),
            identity: nostr_public_key(&*identity_secret(seed, index)?),
        });
    }
    Ok(Addresses {
        connectors: keys,
        operator_write: operator_write_public_key(&*operator_write_secret(seed)?),
        agent_identity: nostr_public_key(&*agent_identity_secret(seed)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Foundry's and Hardhat's default mnemonic, which `toon-client`'s tests use too.
    const ANVIL: &str = "test test test test test test test test test test test junk";

    fn anvil_seed() -> Zeroizing<[u8; 64]> {
        seed(&ANVIL.parse().expect("a valid mnemonic"))
    }

    // These two are `toon-client`'s own vectors (`KeyDerivation.test.ts`): what `anvil`
    // prints for accounts (0) and (3), and the Solana address at index 0.
    #[test]
    fn evm_settlement_agrees_with_toon_client() {
        let seed = anvil_seed();
        let at = |index| evm_address(&evm_settlement_secret(&*seed, index).unwrap());
        assert_eq!(at(0), "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        assert_eq!(at(3), "0x90F79bf6EB2c4f870365E785982E1f101E93b906");
    }

    #[test]
    fn solana_settlement_agrees_with_toon_client() {
        let seed = anvil_seed();
        let address = solana_address(&solana_settlement_secret(&*seed, 0).unwrap());
        assert_eq!(address, "oeYf6KAJkLYhBuR8CiGc6L4D4Xtfepr85fuDgA9kq96");
    }

    // The wallet's own keys have no `toon-client` to agree with, so these pin what this
    // release derived, as an independent BIP-32 implementation computed it. Changing one
    // moves every operator's identity and write key.
    #[test]
    fn the_wallets_own_keys_are_pinned() {
        let derived = addresses(&*anvil_seed(), 2).unwrap();
        assert_eq!(PINNED_AGENT_IDENTITY, derived.agent_identity);
        assert_eq!(PINNED_OPERATOR_WRITE, derived.operator_write);
        assert_eq!(PINNED_IDENTITY_0, derived.connectors[0].identity);
        assert_eq!(PINNED_IDENTITY_1, derived.connectors[1].identity);
    }

    const PINNED_AGENT_IDENTITY: &str =
        "ff535e30a4f7288a270c465f6d8c033b177342d5c5162bda8c4c3ed8e6b3267b";
    // The Ed25519 public key, by `ed25519-dalek`, of the secret pinned by BIP-32 like
    // the others.
    const PINNED_OPERATOR_WRITE: &str =
        "ab202b62ab312a6026db3c651308c445af43983e89ee591f8bf51f7c6aa0756f";
    const PINNED_IDENTITY_0: &str =
        "41c5dadd3b76286c4f4c4b0869b2d05e1c1a61bba8b75b7b884d2b1e1ee04079";
    const PINNED_IDENTITY_1: &str =
        "6392fc73029ed0fdbf6db7029e9a84d35d8adee6490f940009702d17a827a9ad";

    #[test]
    fn the_agent_identity_differs_from_every_settlement_key() {
        let seed = anvil_seed();
        let identity = nostr_public_key(&agent_identity_secret(&*seed).unwrap());
        let identity_secret = agent_identity_secret(&*seed).unwrap();
        for index in 0..8 {
            let evm = evm_settlement_secret(&*seed, index).unwrap();
            let solana = solana_settlement_secret(&*seed, index).unwrap();
            assert_ne!(*identity_secret, *evm);
            assert_ne!(*identity_secret, *solana);
            // Compared as addresses too: the same secret would give the same one.
            assert_ne!(evm_address(&identity_secret), evm_address(&evm));
            assert_ne!(identity, nostr_public_key(&evm));
        }
    }

    #[test]
    fn every_key_of_the_wallet_is_distinct() {
        let seed = anvil_seed();
        let mut secrets = vec![
            agent_identity_secret(&*seed).unwrap(),
            operator_write_secret(&*seed).unwrap(),
        ];
        for index in 0..4 {
            secrets.push(identity_secret(&*seed, index).unwrap());
            secrets.push(evm_settlement_secret(&*seed, index).unwrap());
            secrets.push(solana_settlement_secret(&*seed, index).unwrap());
        }
        for (position, secret) in secrets.iter().enumerate() {
            for other in &secrets[position + 1..] {
                assert_ne!(**secret, **other);
            }
        }
    }

    #[test]
    fn a_connector_index_bip32_cannot_represent_is_refused() {
        assert!(evm_settlement_secret(&*anvil_seed(), 1 << 31).is_err());
        assert!(solana_settlement_secret(&*anvil_seed(), 1 << 31).is_err());
        assert!(identity_secret(&*anvil_seed(), 1 << 31).is_err());
    }
}
