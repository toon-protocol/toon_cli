//! The keystore: the wallet's mnemonic, encrypted at rest.
//!
//! scrypt turns the passphrase into a key, and AES-256-GCM seals the mnemonic with it.
//! The file records its own parameters, so it can be read after they change.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::outcome::{Error, ErrorCode};

/// The environment variable that holds the passphrase.
pub const PASSPHRASE_ENV: &str = "TOON_PASSPHRASE";
/// The environment variable that names a file holding the passphrase.
pub const PASSPHRASE_FILE_ENV: &str = "TOON_PASSPHRASE_FILE";

const LOG_N: u8 = 17;
const R: u32 = 8;
const P: u32 = 1;

pub fn path(home: &Path) -> PathBuf {
    home.join("keystore.json")
}

fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}

/// The passphrase, from a file or an environment variable and never from a flag, so it
/// is not in a process list. The file wins when both are set.
pub fn passphrase() -> Result<Zeroizing<String>, Error> {
    let passphrase = if let Some(file) = env::var_os(PASSPHRASE_FILE_ENV).filter(|f| !f.is_empty())
    {
        let text = fs::read_to_string(&file).map_err(|source| {
            error(
                ErrorCode::PassphraseUnreadable,
                format!(
                    "{PASSPHRASE_FILE_ENV} names {}, which cannot be read: {source}.",
                    Path::new(&file).display()
                ),
            )
        })?;
        // A file written with `echo` ends in a newline that is not part of the passphrase.
        let trimmed = text
            .strip_suffix('\n')
            .map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
            .unwrap_or(&text);
        Zeroizing::new(trimmed.to_owned())
    } else if let Some(value) = env::var_os(PASSPHRASE_ENV) {
        Zeroizing::new(value.into_string().map_err(|_| {
            error(
                ErrorCode::PassphraseUnreadable,
                format!("{PASSPHRASE_ENV} is not valid UTF-8."),
            )
        })?)
    } else {
        return Err(error(
            ErrorCode::PassphraseMissing,
            format!("Set {PASSPHRASE_FILE_ENV} to a file holding the wallet passphrase, or {PASSPHRASE_ENV} to the passphrase itself. A flag is not accepted, because it would show in a process list."),
        ));
    };
    if passphrase.is_empty() {
        return Err(error(
            ErrorCode::PassphraseMissing,
            "The wallet passphrase is empty.",
        ));
    }
    Ok(passphrase)
}

fn derive_key(passphrase: &str, salt: &[u8], log_n: u8) -> Result<Zeroizing<[u8; 32]>, Error> {
    let invalid = || {
        error(
            ErrorCode::KeystoreCorrupt,
            "The keystore's scrypt parameters are not valid.",
        )
    };
    let params = scrypt::Params::new(log_n, R, P, 32).map_err(|_| invalid())?;
    let mut key = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, key.as_mut_slice())
        .map_err(|_| invalid())?;
    Ok(key)
}

/// `N` bytes from the system's randomness.
pub fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|source| error(ErrorCode::Io, format!("No source of randomness: {source}.")))?;
    Ok(bytes)
}

fn io(path: &Path, source: std::io::Error) -> Error {
    error(ErrorCode::Io, format!("{}: {source}.", path.display()))
}

/// Seal `mnemonic` under `passphrase` and write it to a new keystore in `home`.
///
/// Returns `Ok(false)` and writes nothing if a keystore is already there, so two
/// concurrent `init`s cannot make two wallets.
pub fn create(home: &Path, passphrase: &str, mnemonic: &str) -> Result<bool, Error> {
    let salt = random::<16>()?;
    let nonce = random::<12>()?;
    let key = derive_key(passphrase, &salt, LOG_N)?;
    let ciphertext = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()))
        .encrypt(Nonce::from_slice(&nonce), mnemonic.as_bytes())
        .map_err(|_| error(ErrorCode::Io, "The mnemonic could not be encrypted."))?;
    let document = json!({
        "version": 1,
        "kdf": { "name": "scrypt", "log_n": LOG_N, "r": R, "p": P, "salt": hex::encode(salt) },
        "cipher": { "name": "aes-256-gcm", "nonce": hex::encode(nonce) },
        "ciphertext": hex::encode(ciphertext),
    });

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)
        .map_err(|source| io(home, source))?;
    let file = path(home);
    // Written in full to a file of its own first, so a crash leaves no half-written
    // keystore that reads as a wallet that cannot be opened.
    let staged = home.join(format!("keystore.json.{}.tmp", hex::encode(random::<8>()?)));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)
        .and_then(|mut opened| {
            writeln!(opened, "{document:#}")?;
            opened.sync_all()
        });
    if let Err(source) = written {
        let _ = fs::remove_file(&staged);
        return Err(io(&staged, source));
    }
    // A link, unlike a rename, refuses to replace a keystore that is already there.
    let linked = fs::hard_link(&staged, &file);
    let _ = fs::remove_file(&staged);
    match linked {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(source) => Err(io(&file, source)),
    }
}

/// The error for a `home` with no keystore.
pub fn no_wallet(home: &Path) -> Error {
    error(
        ErrorCode::NoWallet,
        format!("There is no wallet at {}. Run `toon init`.", home.display()),
    )
}

/// Whether `home` has a keystore.
pub fn exists(home: &Path) -> bool {
    path(home).exists()
}

/// Open the keystore in `home` and return the mnemonic.
pub fn open(home: &Path, passphrase: &str) -> Result<Zeroizing<String>, Error> {
    let file = path(home);
    let text = fs::read_to_string(&file).map_err(|source| match source.kind() {
        ErrorKind::NotFound => no_wallet(home),
        _ => io(&file, source),
    })?;
    let corrupt = || {
        error(
            ErrorCode::KeystoreCorrupt,
            format!("{} is not a keystore this version reads.", file.display()),
        )
    };
    let document: Value = serde_json::from_str(&text).map_err(|_| corrupt())?;
    let field = |outer: &str, inner: &str| -> Result<Vec<u8>, Error> {
        let text = document[outer][inner].as_str().ok_or_else(corrupt)?;
        hex::decode(text).map_err(|_| corrupt())
    };
    if document["version"] != 1
        || document["kdf"]["name"] != "scrypt"
        || document["cipher"]["name"] != "aes-256-gcm"
    {
        return Err(corrupt());
    }
    let log_n = document["kdf"]["log_n"]
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .filter(|n| (1..=24).contains(n))
        .ok_or_else(corrupt)?;
    let salt = field("kdf", "salt")?;
    let nonce = field("cipher", "nonce")?;
    let ciphertext =
        hex::decode(document["ciphertext"].as_str().ok_or_else(corrupt)?).map_err(|_| corrupt())?;
    if nonce.len() != 12 {
        return Err(corrupt());
    }
    let key = derive_key(passphrase, &salt, log_n)?;
    let plain = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()))
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| {
            error(
                ErrorCode::PassphraseWrong,
                "The wallet passphrase is wrong.",
            )
        })?;
    let mnemonic = String::from_utf8(plain).map_err(|_| corrupt())?;
    Ok(Zeroizing::new(mnemonic))
}
