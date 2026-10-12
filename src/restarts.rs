//! The restart limit of an agent key (ADR 0010).
//!
//! Setting a route's price restarts a connector, and packets fail while it restarts. So an
//! agent key may do it `max_restarts` times an hour (`Key::max_restarts`, in the home's
//! record of agent keys). The supervisor is the one to count: it keeps the time of each
//! restart a key ordered in `agent-key-restarts.json` in the home, so a count survives the
//! supervisor. The operator with the home is never counted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::agent_key::Key;
use crate::node;
use crate::outcome::{Error, ErrorCode};

/// The hour a key's restarts are counted over, in seconds.
const WINDOW: u64 = 3600;

const FILE: &str = "agent-key-restarts.json";

fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

fn load(home: &Path) -> BTreeMap<String, Vec<u64>> {
    std::fs::read_to_string(path(home))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .as_object()?
                .iter()
                .map(|(key, times)| {
                    let times: Option<Vec<u64>> =
                        times.as_array()?.iter().map(Value::as_u64).collect();
                    Some((key.clone(), times?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Count one restart for `key` at `now`, if it is under its limit. Otherwise nothing is
/// counted and the error says when the next restart is allowed.
///
/// Two orders at once do not both pass: the count is read and written under one lock.
pub fn take(home: &Path, key: &Key, now: u64) -> Result<(), Error> {
    static COUNTING: Mutex<()> = Mutex::new(());
    let _counting = COUNTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut all = load(home);
    // Only the keys the record still holds, and only restarts still inside the hour.
    let mut times = all.remove(&key.public).unwrap_or_default();
    times.retain(|at| at + WINDOW > now);
    let limit = usize::try_from(key.max_restarts).unwrap_or(usize::MAX);
    if times.len() >= limit {
        let message = match times.iter().min() {
            Some(oldest) if limit > 0 => {
                let next = i64::try_from(oldest + WINDOW).unwrap_or(i64::MAX);
                let when = chrono::DateTime::from_timestamp(next, 0)
                    .map_or_else(|| next.to_string(), |when| when.to_rfc3339());
                format!(
                    "This agent key has restarted a connector {} times in the last hour, its \
                     limit. The next restart is allowed at {when}. Nothing was changed.",
                    times.len()
                )
            }
            _ => "This agent key may not restart a connector: its restart limit is 0. The \
                  operator changes it with `toon agent-key set --max-restarts`. Nothing was \
                  changed."
                .to_owned(),
        };
        return Err(Error {
            code: ErrorCode::RateLimited,
            message,
            nothing_sent: true,
            unanswered: None,
        });
    }
    times.push(now);
    all.insert(key.public.clone(), times);
    all.retain(|_, times| times.iter().any(|at| at + WINDOW > now));
    let value = json!(all);
    node::write(&path(home), format!("{value}\n").as_bytes(), 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(max_restarts: u32) -> Key {
        Key {
            public: "ab".repeat(32),
            scope: vec!["route-price".into()],
            max_per_command: 0,
            max_per_day: 0,
            max_restarts,
            expires: None,
            toon_apps: vec!["relay".into()],
        }
    }

    #[test]
    fn the_third_restart_in_an_hour_is_refused_with_the_time_of_the_next() {
        let home = tempfile::tempdir().expect("a directory");
        let key = key(2);
        take(home.path(), &key, 1_000_000).expect("first");
        take(home.path(), &key, 1_000_100).expect("second");
        let refused = take(home.path(), &key, 1_000_200).expect_err("third");
        assert_eq!(refused.code, ErrorCode::RateLimited);
        assert!(
            refused.message.contains("1970-01-12T"),
            "{}",
            refused.message
        );
        // An hour after the first, one more is allowed.
        take(home.path(), &key, 1_000_000 + WINDOW).expect("again");
    }
}
