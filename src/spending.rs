//! The spending limit, and the one place every command that moves money goes through.
//!
//! A command that pays calls [`spend`] with its amount. `spend` refuses it without `--yes`,
//! and past the per-command limit or what is left of the day's, and otherwise records the
//! amount against the day before the command runs. The command hands back how much actually
//! moved; what did not is given back.
//!
//! The limits are in `limits.json`, signed with a key derived from the wallet's mnemonic
//! (`derive::limits_secret`). Reading them needs no passphrase, only the public key the file
//! carries; writing them needs the keystore opened, so `toon limit set` needs the passphrase
//! and an agent that holds none cannot raise them. A file whose signature does not verify is
//! refused, so a hand edit of the numbers does not raise a limit. The public key travels in
//! the file, so this stops an edit, not an agent that can also rewrite the whole file with
//! a key of its own: keep the agent's operating-system user apart from the one that owns
//! the home directory if that matters.
//!
//! What was spent today is in `spent.json`, under an advisory lock so that two commands
//! cannot both spend the same remainder.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Value};

use crate::outcome::{Error, ErrorCode, Exit, Report};
use crate::{derive, keystore, node, operator};

/// The most one command may pay, in the token's base units, unless `init` says otherwise:
/// ten tokens at six decimals.
pub const DEFAULT_PER_COMMAND: u128 = 10_000_000;
/// The most a day's commands may pay together, unless `init` says otherwise.
pub const DEFAULT_PER_DAY: u128 = 100_000_000;

const SECONDS_PER_DAY: u64 = 86_400;

/// The spending limit, in the token's base units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub per_command: u128,
    pub per_day: u128,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_command: DEFAULT_PER_COMMAND,
            per_day: DEFAULT_PER_DAY,
        }
    }
}

/// What has been spent on one UTC day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    /// Days since the Unix epoch.
    pub day: u64,
    pub spent: u128,
}

/// Why a payment is over the limit.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    PerCommand { limit: u128 },
    PerDay { limit: u128, remaining: u128 },
}

impl Refusal {
    fn message(&self, amount: u128) -> String {
        match self {
            Refusal::PerCommand { limit } => format!(
                "{amount} is over the per-command spending limit of {limit}. \
                 Only `toon limit set`, with the wallet passphrase, changes it."
            ),
            Refusal::PerDay { limit, remaining } => format!(
                "{amount} is over the remaining daily spending limit: {remaining} of {limit} \
                 remains today. Only `toon limit set`, with the wallet passphrase, changes it."
            ),
        }
    }
}

/// The UTC day a Unix time falls on.
pub fn day_of(unix_seconds: u64) -> u64 {
    unix_seconds / SECONDS_PER_DAY
}

impl Ledger {
    /// What `day` has spent: nothing, if the ledger is of an earlier day.
    fn on(self, day: u64) -> Ledger {
        if self.day == day {
            self
        } else {
            Ledger { day, spent: 0 }
        }
    }

    /// What a day's limit has left.
    pub fn remaining(self, limits: &Limits, day: u64) -> u128 {
        limits.per_day.saturating_sub(self.on(day).spent)
    }

    /// The ledger after `amount` is spent on `day`, or why it may not be.
    pub fn charge(self, limits: &Limits, day: u64, amount: u128) -> Result<Ledger, Refusal> {
        if amount > limits.per_command {
            return Err(Refusal::PerCommand {
                limit: limits.per_command,
            });
        }
        let today = self.on(day);
        let remaining = limits.per_day.saturating_sub(today.spent);
        if amount > remaining {
            return Err(Refusal::PerDay {
                limit: limits.per_day,
                remaining,
            });
        }
        Ok(Ledger {
            day,
            spent: today.spent + amount,
        })
    }

    /// The ledger with a charge of `amount` on `day` taken back.
    fn refund(self, day: u64, amount: u128) -> Ledger {
        if self.day == day {
            Ledger {
                day,
                spent: self.spent.saturating_sub(amount),
            }
        } else {
            self
        }
    }
}

fn today() -> u64 {
    day_of(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

fn limit_error(message: impl Into<String>) -> Error {
    Error {
        nothing_sent: false,
        code: ErrorCode::SpendingLimit,
        message: message.into(),
    }
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error {
        nothing_sent: false,
        code: ErrorCode::Io,
        message: format!("{}: {source}.", path.display()),
    }
}

pub fn limits_path(home: &Path) -> PathBuf {
    home.join("limits.json")
}

fn ledger_path(home: &Path) -> PathBuf {
    home.join("spent.json")
}

fn lock_path(home: &Path) -> PathBuf {
    home.join("spent.lock")
}

fn signed_text(limits: &Limits) -> String {
    format!(
        "toon spending limit v1\nper_command={}\nper_day={}",
        limits.per_command, limits.per_day
    )
}

/// Write the limits, signed with the key the wallet's seed derives. Used with a seed in
/// hand: at `init`, and at `limit set` once the passphrase has opened the keystore.
pub fn write_limits(home: &Path, seed: &[u8], limits: &Limits) -> Result<(), Error> {
    let secret = derive::limits_secret(seed).map_err(|source| Error {
        nothing_sent: false,
        code: ErrorCode::KeystoreCorrupt,
        message: source.0,
    })?;
    let key = SigningKey::from_bytes(&secret);
    let signature = key.sign(signed_text(limits).as_bytes());
    let document = json!({
        "per_command": limits.per_command.to_string(),
        "per_day": limits.per_day.to_string(),
        "public_key": hex::encode(key.verifying_key().as_bytes()),
        "signature": hex::encode(signature.to_bytes()),
    });
    node::write(
        &limits_path(home),
        format!("{document:#}\n").as_bytes(),
        0o600,
    )
}

/// The limits, checked against their signature. A missing or altered file is refused,
/// which stops every payment.
pub fn read_limits(home: &Path) -> Result<Limits, Error> {
    let file = limits_path(home);
    let text = fs::read_to_string(&file).map_err(|source| match source.kind() {
        ErrorKind::NotFound => limit_error(format!(
            "There is no spending limit at {}. Set one with `toon limit set`.",
            file.display()
        )),
        _ => io(&file, source),
    })?;
    let altered = || {
        limit_error(format!(
            "{} is not a spending limit this wallet signed. Set one with `toon limit set`.",
            file.display()
        ))
    };
    let document: Value = serde_json::from_str(&text).map_err(|_| altered())?;
    let number = |name: &str| -> Result<u128, Error> {
        document[name]
            .as_str()
            .and_then(|text| text.parse().ok())
            .ok_or_else(altered)
    };
    let limits = Limits {
        per_command: number("per_command")?,
        per_day: number("per_day")?,
    };
    let bytes = |name: &str| -> Result<Vec<u8>, Error> {
        hex::decode(document[name].as_str().ok_or_else(altered)?).map_err(|_| altered())
    };
    let public: [u8; 32] = bytes("public_key")?.try_into().map_err(|_| altered())?;
    let signature: [u8; 64] = bytes("signature")?.try_into().map_err(|_| altered())?;
    VerifyingKey::from_bytes(&public)
        .and_then(|key| {
            key.verify(
                signed_text(&limits).as_bytes(),
                &Signature::from_bytes(&signature),
            )
        })
        .map_err(|_| altered())?;
    Ok(limits)
}

fn read_ledger(home: &Path) -> Result<Ledger, Error> {
    let file = ledger_path(home);
    match fs::read_to_string(&file) {
        Ok(text) => {
            let corrupt = || {
                limit_error(format!(
                    "{} is not a record of spending this version reads.",
                    file.display()
                ))
            };
            let document: Value = serde_json::from_str(&text).map_err(|_| corrupt())?;
            Ok(Ledger {
                day: document["day"].as_u64().ok_or_else(corrupt)?,
                spent: document["spent"]
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or_else(corrupt)?,
            })
        }
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(Ledger::default()),
        Err(source) => Err(io(&file, source)),
    }
}

fn write_ledger(home: &Path, ledger: Ledger) -> Result<(), Error> {
    let document = json!({ "day": ledger.day, "spent": ledger.spent.to_string() });
    node::write(
        &ledger_path(home),
        format!("{document}\n").as_bytes(),
        0o600,
    )
}

/// Run `change` on the ledger while holding the lock that serialises spenders.
fn locked<T>(home: &Path, change: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    locked_by(&lock_path(home), change)
}

/// Run `change` while holding the advisory lock at `file`.
fn locked_by<T>(file: &Path, change: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let lock: File = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(file)
        .map_err(|source| io(file, source))?;
    lock.lock().map_err(|source| io(file, source))?;
    let result = change();
    let _ = lock.unlock();
    result
}

/// How much of what a command was counted for actually moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// All of it, or as much as may have.
    All,
    /// None of it.
    Nothing,
    /// This much, which is at most what was counted.
    Amount(u128),
}

impl From<bool> for Moved {
    fn from(moved: bool) -> Self {
        if moved {
            Moved::All
        } else {
            Moved::Nothing
        }
    }
}

impl From<u128> for Moved {
    fn from(amount: u128) -> Self {
        Moved::Amount(amount)
    }
}

impl Moved {
    /// What stays counted out of `amount`.
    fn of(self, amount: u128) -> u128 {
        match self {
            Moved::All => amount,
            Moved::Nothing => 0,
            Moved::Amount(moved) => moved.min(amount),
        }
    }
}

/// What a money-moving command did: its report, and how much money actually moved. A
/// payment that was rejected may have moved none, and then it is not counted.
pub type Spent<T, M = bool> = Result<(T, M), Error>;

/// The one gate for a command that moves `amount` base units: it needs `--yes`, it must
/// fit the per-command limit and what is left of the day's, and it is counted before
/// `act` runs so that a second command cannot spend the same remainder. What `act` says
/// did not move, or what it failed before it could have paid, is given back.
pub fn spend<T, M: Into<Moved> + Copy>(
    home: &Path,
    amount: u128,
    yes: bool,
    act: impl FnOnce() -> Spent<T, M>,
) -> Result<T, Error> {
    spend_with(home, amount, yes, act)
}

fn spend_with<T, M: Into<Moved> + Copy>(
    home: &Path,
    amount: u128,
    yes: bool,
    act: impl FnOnce() -> Spent<T, M>,
) -> Result<T, Error> {
    if !yes {
        return Err(Error {
            nothing_sent: false,
            code: ErrorCode::NotConfirmed,
            message: format!(
                "This moves {amount} base units. Add `--yes` to say that you mean it."
            ),
        });
    }
    if node::State::load(home)?.is_none() {
        return Err(node::no_agent_node(home));
    }
    let limits = read_limits(home)?;
    let day = today();
    locked(home, || {
        let charged = read_ledger(home)?
            .charge(&limits, day, amount)
            .map_err(|refusal| limit_error(refusal.message(amount)))?;
        write_ledger(home, charged)
    })?;
    let outcome = act();
    let kept = match &outcome {
        Ok((_, moved)) => (*moved).into().of(amount),
        Err(error) if failed_before_paying(error) => 0,
        Err(_) => amount,
    };
    if kept < amount {
        // The rest is free again. A failure to take it back leaves it counted, which
        // errs on the side of the limit.
        let _ = locked(home, || {
            write_ledger(home, read_ledger(home)?.refund(day, amount - kept))
        });
    }
    outcome.map(|(report, _)| report)
}

/// What the outbound channels' watermarks were before a packet, read under the lock that
/// keeps another `toon` command's packet from moving them meanwhile.
pub struct Packets<'a> {
    home: &'a Path,
    before: Option<u128>,
}

impl Packets<'_> {
    /// What the packets sent since moved the watermarks by, at most `cap`; `cap` when the
    /// watermarks cannot be read, which errs on the side of the limit.
    pub fn moved(&self, cap: u128) -> u128 {
        let after = operator::outbound_watermark(self.home).ok();
        match (self.before, after) {
            (Some(before), Some(after)) => after.saturating_sub(before).min(cap),
            _ => cap,
        }
    }
}

/// [`spend`] for a command that sends packets. `act` is given the watermarks as they stand
/// before, and no other packet-sending command of this agent node runs until it returns, so
/// that what moved across it is its own.
pub fn spend_packets<T, M: Into<Moved> + Copy>(
    home: &Path,
    amount: u128,
    yes: bool,
    act: impl FnOnce(&Packets) -> Spent<T, M>,
) -> Result<T, Error> {
    spend_with(home, amount, yes, || {
        locked_by(&home.join("packets.lock"), || {
            act(&Packets {
                home,
                before: operator::outbound_watermark(home).ok(),
            })
        })
    })
}

/// Whether a command that failed with `error` certainly paid nothing: it never reached the
/// connector, the settlement key was refused as `unfunded` before anything was sent, the
/// other side refused the peering before a channel was opened, or it failed before it sent
/// a packet (`nothing_sent`). Any other failure, a timeout or an answer not understood, may
/// come after the money moved, so it stays counted.
fn failed_before_paying(error: &Error) -> bool {
    error.nothing_sent
        || matches!(
            error.code,
            ErrorCode::NoAgentNode
                | ErrorCode::NotRunning
                | ErrorCode::Unfunded
                | ErrorCode::PeerNotPeerable
        )
}

/// `toon limit show`: the limits, and what is left of today's.
pub fn show(home: &Path) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let limits = read_limits(home)?;
    let day = today();
    let remaining = locked(home, || read_ledger(home))?.remaining(&limits, day);
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "limits": {
            "per_command": limits.per_command.to_string(),
            "per_day": limits.per_day.to_string(),
            "remaining_today": remaining.to_string(),
        } }),
        text: format!(
            "Per command: {}. Per day: {}. Remaining today: {remaining}. Amounts are in the token's base units.",
            limits.per_command, limits.per_day
        ),
    })
}

/// `toon limit set`: change either limit. Opens the keystore, so it needs the passphrase.
pub fn set(home: &Path, per_command: Option<u128>, per_day: Option<u128>) -> Result<Report, Error> {
    if !keystore::exists(home) {
        return Err(keystore::no_wallet(home));
    }
    let passphrase = keystore::passphrase()?;
    let mnemonic: bip39::Mnemonic =
        keystore::open(home, &passphrase)?
            .parse()
            .map_err(|_| Error {
                nothing_sent: false,
                code: ErrorCode::KeystoreCorrupt,
                message: "The keystore does not hold a valid mnemonic.".into(),
            })?;
    // A limit not given keeps what the file holds. A file that is missing or was altered
    // holds nothing to keep, so then both are needed: a default could loosen a limit.
    let limits = match (per_command, per_day) {
        (Some(per_command), Some(per_day)) => Limits {
            per_command,
            per_day,
        },
        _ => {
            let current = read_limits(home).map_err(|error| match error.code {
                ErrorCode::SpendingLimit => limit_error(format!(
                    "{} Give both `--max-per-command` and `--max-per-day`.",
                    error.message
                )),
                _ => error,
            })?;
            Limits {
                per_command: per_command.unwrap_or(current.per_command),
                per_day: per_day.unwrap_or(current.per_day),
            }
        }
    };
    write_limits(home, &*derive::seed(&mnemonic), &limits)?;
    Ok(Report {
        exit: Exit::Success,
        json: json!({ "limits": {
            "per_command": limits.per_command.to_string(),
            "per_day": limits.per_day.to_string(),
        } }),
        text: format!(
            "Spending limit set: {} per command, {} per day.",
            limits.per_command, limits.per_day
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        per_command: 100,
        per_day: 250,
    };

    #[test]
    fn a_payment_within_both_limits_is_counted() {
        let ledger = Ledger::default().charge(&LIMITS, 5, 100).unwrap();
        assert_eq!(ledger, Ledger { day: 5, spent: 100 });
        assert_eq!(ledger.remaining(&LIMITS, 5), 150);
    }

    #[test]
    fn a_payment_over_the_per_command_limit_is_refused_whatever_the_day_has_left() {
        assert_eq!(
            Ledger::default().charge(&LIMITS, 5, 101),
            Err(Refusal::PerCommand { limit: 100 })
        );
    }

    #[test]
    fn a_payment_over_what_the_day_has_left_says_how_much_is_left() {
        let ledger = Ledger { day: 5, spent: 200 };
        assert_eq!(
            ledger.charge(&LIMITS, 5, 51),
            Err(Refusal::PerDay {
                limit: 250,
                remaining: 50
            })
        );
        assert!(ledger.charge(&LIMITS, 5, 50).is_ok());
    }

    #[test]
    fn the_day_is_spent_to_the_last_unit() {
        let ledger = Ledger { day: 5, spent: 250 };
        assert_eq!(ledger.remaining(&LIMITS, 5), 0);
        assert!(ledger.charge(&LIMITS, 5, 1).is_err());
        assert!(ledger.charge(&LIMITS, 5, 0).is_ok());
    }

    #[test]
    fn a_new_day_starts_with_the_whole_limit() {
        let ledger = Ledger { day: 5, spent: 250 };
        assert_eq!(ledger.remaining(&LIMITS, 6), 250);
        assert_eq!(
            ledger.charge(&LIMITS, 6, 100).unwrap(),
            Ledger { day: 6, spent: 100 }
        );
    }

    #[test]
    fn the_day_changes_at_midnight_utc() {
        assert_eq!(day_of(0), 0);
        assert_eq!(day_of(86_399), 0);
        assert_eq!(day_of(86_400), 1);
        assert_eq!(day_of(2 * 86_400 - 1), 1);
    }

    #[test]
    fn a_refund_takes_back_only_that_days_charge() {
        let ledger = Ledger { day: 5, spent: 150 };
        assert_eq!(ledger.refund(5, 100), Ledger { day: 5, spent: 50 });
        assert_eq!(ledger.refund(4, 100), ledger);
        assert_eq!(ledger.refund(5, 900), Ledger { day: 5, spent: 0 });
    }

    #[test]
    fn a_limit_of_zero_refuses_every_payment_of_something() {
        let none = Limits {
            per_command: 0,
            per_day: 0,
        };
        assert!(Ledger::default().charge(&none, 1, 1).is_err());
        assert!(Ledger::default().charge(&none, 1, 0).is_ok());
    }

    #[test]
    fn only_a_failure_before_paying_frees_the_amount() {
        let failure = |code| Error {
            nothing_sent: false,
            code,
            message: String::new(),
        };
        assert!(failed_before_paying(&failure(ErrorCode::NotRunning)));
        assert!(failed_before_paying(&failure(ErrorCode::PeerNotPeerable)));
        assert!(failed_before_paying(&failure(ErrorCode::Unfunded)));
        assert!(!failed_before_paying(&failure(ErrorCode::SendFailed)));
        let mut unsent = failure(ErrorCode::SendFailed);
        unsent.nothing_sent = true;
        assert!(failed_before_paying(&unsent));
        assert!(!failed_before_paying(&failure(ErrorCode::PeerFailed)));
    }
}
