//! The supervisor's receiving end of every subscription (ADR 0005).
//!
//! For each relay the operator holds a subscription at, one worker dials that relay's paid
//! live feed (through the overlay's proxy when the supervisor has one) and hands each event
//! to the agent node's own relay at its write endpoint, over loopback. The relay verifies,
//! stores and broadcasts it as it does any write.
//!
//! What to receive is what `toon relay subscribe` kept in `subscriptions.json`, read again
//! every tick: a subscription that has a balance is received, so a restart resumes them, a
//! top-up resumes one that ran out, and a new filter replaces the old one. A feed that
//! drops is dialled again after a delay that doubles; one that ends with `payment-required`
//! is marked exhausted and left alone until the balance is topped up.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::feed::{self, Ended};
use crate::subscribe::{self, Kept};

/// How often the receiver reads what is kept again.
const TICK: Duration = Duration::from_millis(250);

/// How long a feed that dropped waits before it is dialled again, and the longest wait.
const FIRST_RETRY: Duration = Duration::from_millis(500);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// A feed that stayed up this long was healthy, so its next retry starts over.
const HEALTHY: Duration = Duration::from_secs(30);

/// A relay broadcasts in the order it accepts events and not in the order of `created_at`,
/// so a feed that is dialled again asks for what it may have missed from this long before
/// the newest event it has seen.
const OVERLAP: u64 = 600;

/// Where the supervisor's own relay and the overlay's proxy are, as they are now.
pub trait Surroundings: Send + Sync {
    /// The write address of the agent node's own relay, if it runs.
    fn relay(&self) -> Option<SocketAddr>;
    /// The overlay's SOCKS5 proxy, if the supervisor has one.
    fn proxy(&self) -> Option<SocketAddr>;
}

/// The receiving end of the subscriptions; it stops when it is dropped.
pub struct Receiver {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Receiver {
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

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One relay's worker.
struct Worker {
    /// The filter it was started with: a new one replaces the worker.
    filter: Value,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    /// When it was started, and so how long it ran when it has ended.
    since: Instant,
}

/// What the worker of a relay that ended is waited on for.
struct Retry {
    after: Instant,
    delay: Duration,
}

/// What a relay's feed has delivered, kept across the workers that read it one after the
/// other.
#[derive(Default)]
struct Progress {
    /// The newest `created_at` handed on.
    newest: Option<u64>,
    /// The ids handed on, so an event the feed sends again is not handed on again.
    seen: HashSet<String>,
}

fn run(home: &Path, surroundings: &dyn Surroundings, stop: &AtomicBool) {
    let mut workers: HashMap<String, Worker> = HashMap::new();
    let mut retries: HashMap<String, Retry> = HashMap::new();
    let mut progress: HashMap<String, Arc<Mutex<Progress>>> = HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(TICK);
        let kept = subscribe::load(home).unwrap_or_default();

        // A worker that has ended is waited on; one whose subscription changed or ran out
        // is stopped.
        for relay in workers.keys().cloned().collect::<Vec<_>>() {
            let worker = &workers[&relay];
            let replaced = kept
                .iter()
                .find(|kept| kept.relay == relay)
                .is_none_or(|kept| kept.exhausted() || worker.filter != kept.filter);
            if replaced {
                worker.stop.store(true, Ordering::SeqCst);
            }
            if worker.thread.is_finished() || replaced {
                let worker = workers.remove(&relay).expect("a worker");
                let ran = worker.since.elapsed();
                let _ = worker.thread.join();
                if !replaced {
                    let delay = match retries.get(&relay) {
                        Some(retry) if ran < HEALTHY => (retry.delay * 2).min(LONGEST_RETRY),
                        _ => FIRST_RETRY,
                    };
                    retries.insert(
                        relay,
                        Retry {
                            after: Instant::now() + delay,
                            delay,
                        },
                    );
                }
            }
        }
        retries.retain(|relay, _| kept.iter().any(|kept| &kept.relay == relay));
        progress.retain(|relay, _| kept.iter().any(|kept| &kept.relay == relay));

        let Some(secret) = subscribe::receiving_secret(home) else {
            continue;
        };
        for entry in kept {
            if entry.exhausted()
                || workers.contains_key(&entry.relay)
                || retries
                    .get(&entry.relay)
                    .is_some_and(|retry| Instant::now() < retry.after)
            {
                continue;
            }
            let Some(relay_address) = surroundings.relay() else {
                continue;
            };
            let relay = entry.relay.clone();
            let filter = entry.filter.clone();
            let progress = Arc::clone(progress.entry(relay.clone()).or_default());
            let stopping = Arc::new(AtomicBool::new(false));
            let thread = {
                let (home, proxy) = (home.to_path_buf(), surroundings.proxy());
                let stopping = Arc::clone(&stopping);
                thread::spawn(move || {
                    receive(
                        &home,
                        &entry,
                        &secret,
                        proxy,
                        relay_address,
                        &stopping,
                        &progress,
                    )
                })
            };
            workers.insert(
                relay,
                Worker {
                    filter,
                    stop: stopping,
                    thread,
                    since: Instant::now(),
                },
            );
        }
    }
    for worker in workers.values() {
        worker.stop.store(true, Ordering::SeqCst);
    }
    for (_, worker) in workers {
        let _ = worker.thread.join();
    }
}

/// Read one relay's feed until it ends, handing each event to the relay at `own`.
fn receive(
    home: &Path,
    entry: &Kept,
    secret: &[u8; 32],
    proxy: Option<SocketAddr>,
    own: SocketAddr,
    stop: &AtomicBool,
    progress: &Mutex<Progress>,
) {
    let mut filter = entry.filter.clone();
    if let (Some(newest), Some(filter)) = (
        progress.lock().expect("the progress").newest,
        filter.as_object_mut(),
    ) {
        let from = newest.saturating_sub(OVERLAP);
        let since = filter.get("since").and_then(Value::as_u64).unwrap_or(0);
        filter.insert("since".into(), from.max(since).into());
    }
    let ended = feed::read(&entry.relay, secret, &filter, proxy, stop, |event| {
        let mut progress = progress.lock().expect("the progress");
        let id = event["id"].as_str().unwrap_or_default().to_owned();
        if progress.seen.contains(&id) {
            return true;
        }
        // An event the relay did not take is not marked as seen, so it comes again.
        if hand_over(own, &event) {
            progress.seen.insert(id);
            let created = event["created_at"].as_u64();
            progress.newest = progress.newest.max(created);
        }
        true
    });
    if let Ended::Exhausted(_) = ended {
        let _ = subscribe::mark_exhausted(home, &entry.relay);
    }
}

/// Write `event` to the relay at `own`: its write endpoint, or its ephemeral one for an
/// event that is not stored. Whether the relay took it.
fn hand_over(own: SocketAddr, event: &Value) -> bool {
    let path = match event["kind"].as_u64() {
        Some(20000..=29999) => "/write-ephemeral",
        _ => "/write",
    };
    let body = event.to_string();
    let Ok(mut stream) = TcpStream::connect_timeout(&own, Duration::from_secs(5)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: relay\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse::<u16>().ok())
            .is_some_and(|status| (200..300).contains(&status))
}
