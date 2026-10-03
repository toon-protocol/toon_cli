//! The supervisor's inbox: the private messages (NIP-17) that reach the agent node's own
//! relay, opened and kept (ADR 0008).
//!
//! While the supervisor runs and the agent identity's secret is kept, one worker reads the
//! agent node's own relay, and only that relay, for the gift wraps (kind 1059) addressed to
//! the agent identity. It asks for every one, with no `since`, since NIP-59 backdates a
//! wrap by up to two days, and skips the wraps it has already looked at. The rumor of each
//! wrap that opens is kept in `private-messages.json` in the agent node's home. The store
//! is a cache: the wraps on the relay are the record, and a store that is gone is made
//! again on the next start.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::event;
use crate::feed::{self, Ended};
use crate::gift_wrap;
use crate::node::{self, AppFiles};
use crate::outcome::Error;
use crate::receive::Surroundings;

/// How often the worker looks again for a kept secret and a relay to read.
const TICK: Duration = Duration::from_millis(250);

/// How long a read that ended waits before it is made again, and the longest wait.
const FIRST_RETRY: Duration = Duration::from_millis(500);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// A read that stayed up this long was healthy, so its next retry starts over.
const HEALTHY: Duration = Duration::from_secs(30);

/// Where the opened rumors are kept.
pub fn store_path(home: &Path) -> PathBuf {
    home.join("private-messages.json")
}

/// The rumors in the store, in no order. A store that is missing or not readable holds none.
pub fn load(home: &Path) -> Vec<Value> {
    std::fs::read(store_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<Value>>(&bytes).ok())
        .unwrap_or_default()
}

/// Keep `rumor` in the store, readable by the owner alone, if no rumor of its id is kept.
fn keep(home: &Path, rumor: &Value) -> Result<(), Error> {
    let mut rumors = load(home);
    if rumors.iter().any(|kept| kept["id"] == rumor["id"]) {
        return Ok(());
    }
    rumors.push(rumor.clone());
    node::write(
        &store_path(home),
        Value::Array(rumors).to_string().as_bytes(),
        0o600,
    )
}

/// The agent identity's secret as it was kept for the supervisor, if it was.
pub fn kept_secret(home: &Path) -> Option<[u8; 32]> {
    std::fs::read(event::agent_key_path(home))
        .ok()?
        .try_into()
        .ok()
}

/// The keys a rumor names: its author and every key of its `p` tags, sorted, each once.
pub fn participants(rumor: &Value) -> Vec<String> {
    let is_key = |key: &str| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit());
    let mut keys: Vec<String> = rumor["pubkey"]
        .as_str()
        .into_iter()
        .chain(
            rumor["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|tag| tag[0] == "p")
                .filter_map(|tag| tag[1].as_str()),
        )
        .filter(|key| is_key(key))
        .map(str::to_ascii_lowercase)
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The conversation of `participants`: the SHA-256, in hex, of the keys joined with commas.
pub fn conversation(participants: &[String]) -> String {
    hex::encode(Sha256::digest(participants.join(",").as_bytes()))
}

/// The worker that opens the wraps; it stops when it is dropped.
pub struct Inbox {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Inbox {
    pub fn start(home: &Path, surroundings: Arc<dyn Surroundings>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (home, stop) = (home.to_path_buf(), Arc::clone(&stop));
            thread::spawn(move || run(&home, &*surroundings, &stop))
        };
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(home: &Path, surroundings: &dyn Surroundings, stop: &AtomicBool) {
    // Every wrap looked at, whether it opened or not, so that a read made again does not
    // open it, or log it, a second time.
    let mut seen: HashSet<String> = HashSet::new();
    let mut retry = (Instant::now(), FIRST_RETRY);
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(TICK);
        if Instant::now() < retry.0 {
            continue;
        }
        let (Some(secret), Some(address)) = (kept_secret(home), surroundings.read_relay()) else {
            continue;
        };
        let Some(identity_key) = relay_identity(home) else {
            continue;
        };
        let url = reached_at(home).unwrap_or_else(|| format!("ws://{address}"));
        let Ok(identity) = event::public_key(&secret) else {
            continue;
        };
        let started = Instant::now();
        read(
            home,
            address,
            &url,
            &identity_key,
            &secret,
            &identity,
            stop,
            &mut seen,
        );
        let delay = if started.elapsed() < HEALTHY {
            (retry.1 * 2).min(LONGEST_RETRY)
        } else {
            FIRST_RETRY
        };
        retry = (Instant::now() + delay, delay);
    }
}

/// The relay's own identity key, which it takes as an operator's.
fn relay_identity(home: &Path) -> Option<[u8; 32]> {
    std::fs::read(AppFiles::of(home, node::RELAY).identity_key)
        .ok()?
        .try_into()
        .ok()
}

/// Where the agent node's own relay is reached, which its operator's answer to a challenge
/// names, if the agent node says.
fn reached_at(home: &Path) -> Option<String> {
    let state = node::State::load(home).ok()??;
    let app = crate::subscribe::own_relay_app(&state).ok()?;
    node::reached_at(&app.reach, node::onion_endpoint(home, app).as_deref())
}

/// Read the wraps of `identity` at the relay at `address`, reached at `url`, until the read
/// ends.
#[allow(clippy::too_many_arguments)]
fn read(
    home: &Path,
    address: SocketAddr,
    url: &str,
    identity_key: &[u8; 32],
    secret: &[u8; 32],
    identity: &str,
    stop: &AtomicBool,
    seen: &mut HashSet<String>,
) {
    let filter = json!({ "kinds": [gift_wrap::WRAP], "#p": [identity] });
    let ended = feed::read_own(
        &format!("ws://{address}"),
        url,
        identity_key,
        &filter,
        stop,
        |wrap| open(home, secret, identity, &wrap, seen),
    );
    if let Ended::Dropped(why) | Ended::Closed(why) = ended {
        eprintln!("The inbox's read of the own relay ended: {why}");
    }
}

/// Open `wrap` and keep its rumor. A wrap that is not for `identity`, that does not open or
/// that fails NIP-17's check is skipped and logged: it never stops the supervisor.
fn open(home: &Path, secret: &[u8; 32], identity: &str, wrap: &Value, seen: &mut HashSet<String>) {
    let Some(id) = wrap["id"].as_str().map(str::to_owned) else {
        return;
    };
    if !seen.insert(id.clone()) {
        return;
    }
    let for_identity = wrap["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|tag| tag[0] == "p" && tag[1] == identity);
    if !for_identity {
        eprintln!("Skipped wrap {id}: it is not addressed to the agent identity.");
        return;
    }
    match gift_wrap::open(wrap, secret) {
        Ok(opened) => {
            if let Err(error) = keep(home, &opened.rumor) {
                // Looked at again on the next read.
                seen.remove(&id);
                eprintln!(
                    "The message of wrap {id} could not be kept: {}",
                    error.message
                );
            }
        }
        Err(why) => eprintln!("Skipped wrap {id}: {}", why.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participants_are_the_author_and_the_p_tags_sorted_once() {
        let (a, b, c) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        let rumor = json!({
            "pubkey": b,
            "tags": [["p", c], ["p", a], ["p", c], ["subject", "x"], ["p", "nope"]],
        });
        assert_eq!(participants(&rumor), [a, b, c]);
    }

    #[test]
    fn a_conversation_is_the_hash_of_its_keys() {
        let keys = ["aa".repeat(32), "bb".repeat(32)];
        let expected = hex::encode(Sha256::digest(format!("{},{}", keys[0], keys[1])));
        assert_eq!(conversation(&keys), expected);
        assert_ne!(conversation(&keys[..1]), expected);
    }

    fn key(byte: u8) -> ([u8; 32], String) {
        let secret = [byte; 32];
        (secret, event::public_key(&secret).unwrap())
    }

    #[test]
    fn a_forged_wrap_is_skipped_and_the_next_one_is_kept() {
        let home = tempfile::tempdir().unwrap();
        let (sender, sender_key) = key(3);
        let (_, other_key) = key(4);
        let (secret, identity) = key(5);
        let forged = gift_wrap::rumor(&other_key, 1, 14, json!([["p", identity]]), "forged");
        let honest = gift_wrap::rumor(&sender_key, 2, 14, json!([["p", identity]]), "honest");
        let mut seen = HashSet::new();

        for rumor in [&forged, &honest] {
            let wrap = gift_wrap::wrap(rumor, &sender, &identity, 2).unwrap();
            open(home.path(), &secret, &identity, &wrap, &mut seen);
        }

        assert_eq!(load(home.path()), [honest]);
    }

    #[test]
    fn a_rumor_that_arrives_in_two_wraps_is_kept_once() {
        let home = tempfile::tempdir().unwrap();
        let (sender, sender_key) = key(3);
        let (secret, identity) = key(5);
        let rumor = gift_wrap::rumor(&sender_key, 2, 14, json!([["p", identity]]), "twice");
        let mut seen = HashSet::new();

        for _ in 0..2 {
            let wrap = gift_wrap::wrap(&rumor, &sender, &identity, 2).unwrap();
            open(home.path(), &secret, &identity, &wrap, &mut seen);
        }

        assert_eq!(seen.len(), 2);
        assert_eq!(load(home.path()), [rumor]);
    }
}
