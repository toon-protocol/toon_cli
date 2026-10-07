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
//!
//! What each subscription's feed is doing is kept in memory, in `Feeds`, and the supervisor
//! gives it to `status` (the term is "receive", `CONTEXT.md`). None of it is written to disk.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::event;
use crate::feed::{self, Ended};
use crate::overlay;
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
    /// The read address of the agent node's own relay, if it runs.
    fn read_relay(&self) -> Option<SocketAddr>;
    /// The overlay's SOCKS5 proxy, if the supervisor has one. A feed asks for it through
    /// `proxy_for`, which leaves out a relay on this machine.
    fn proxy(&self) -> Option<SocketAddr>;
}

/// The proxy a feed at `url` is dialled through: none for a plain relay on this machine
/// (`overlay::is_local_plain`, the rule a command follows), the overlay's otherwise.
fn proxy_for(surroundings: &dyn Surroundings, url: &str) -> Option<SocketAddr> {
    if overlay::is_local_plain(url) {
        None
    } else {
        surroundings.proxy()
    }
}

/// What the supervisor knows of one subscription's feed.
struct Feed {
    state: &'static str,
    /// When it entered the state, in unix seconds.
    since: u64,
    /// When the agent node's own relay last accepted an event from the feed.
    last_event_at: Option<u64>,
    /// The reason the feed last ended or a hand-over last failed, with its time.
    last_error: Option<(u64, String)>,
}

/// The state of every subscription's feed, by relay URL, shared by the workers that change
/// it and the supervisor that reports it.
#[derive(Clone, Default)]
pub struct Feeds(Arc<Mutex<HashMap<String, Feed>>>);

impl Feeds {
    fn with(&self, relay: &str, change: impl FnOnce(&mut Feed)) {
        let mut feeds = self.0.lock().expect("the feeds");
        let feed = feeds.entry(relay.to_owned()).or_insert_with(|| Feed {
            state: "connecting",
            since: event::now(),
            last_event_at: None,
            last_error: None,
        });
        change(feed);
    }

    fn enter(&self, relay: &str, state: &'static str) {
        self.with(relay, |feed| {
            if feed.state != state {
                feed.state = state;
                feed.since = event::now();
            }
        });
    }

    /// The feed is waiting to try again, for `why`. Saying the same again changes nothing.
    fn retrying(&self, relay: &str, why: &str) {
        self.with(relay, |feed| {
            let same = feed.state == "retrying"
                && feed
                    .last_error
                    .as_ref()
                    .is_some_and(|(_, last)| last == why);
            if !same {
                feed.state = "retrying";
                feed.since = event::now();
                feed.last_error = Some((event::now(), why.to_owned()));
            }
        });
    }

    fn failed(&self, relay: &str, why: &str) {
        self.with(relay, |feed| {
            feed.last_error = Some((event::now(), why.to_owned()));
        });
    }

    fn accepted(&self, relay: &str) {
        self.with(relay, |feed| feed.last_event_at = Some(event::now()));
    }

    fn forget_except(&self, kept: &[Kept]) {
        self.0
            .lock()
            .expect("the feeds")
            .retain(|relay, _| kept.iter().any(|kept| &kept.relay == relay));
    }

    /// Every feed as `status` answers it, by relay URL.
    pub fn json(&self) -> Value {
        let feeds = self.0.lock().expect("the feeds");
        Value::Object(
            feeds
                .iter()
                .map(|(relay, feed)| {
                    (
                        relay.clone(),
                        json!({
                            "state": feed.state,
                            "since": feed.since,
                            "last_event_at": feed.last_event_at,
                            "last_error": feed.last_error.as_ref()
                                .map(|(at, message)| json!({ "at": at, "message": message })),
                        }),
                    )
                })
                .collect(),
        )
    }
}

/// The feed of `relay` as the supervisor's answer to `status` says: null when no supervisor
/// answered. A subscription the supervisor has not looked at yet is connecting.
pub fn feed_of(reply: Option<&Value>, relay: &str) -> Value {
    let Some(reply) = reply else {
        return Value::Null;
    };
    match reply["subscriptions"].get(relay) {
        Some(feed) if feed.is_object() => feed.clone(),
        _ => json!({
            "state": "connecting", "since": event::now(), "last_event_at": null,
            "last_error": null,
        }),
    }
}

/// A unix time as text.
pub fn time(seconds: u64) -> String {
    let now = event::now();
    if seconds > now {
        format!("{seconds} (unix time)")
    } else {
        format!("{seconds} (unix time, {}s ago)", now - seconds)
    }
}

/// What a feed is doing, as a phrase for `status`: a null feed is one no supervisor reports.
pub fn describe_feed(feed: &Value) -> String {
    if feed.is_null() {
        return "not received: the supervisor is not running".to_owned();
    }
    let mut text = feed["state"].as_str().unwrap_or("state unknown").to_owned();
    text.push_str(&match feed["last_event_at"].as_u64() {
        Some(at) => format!(", last event at {}", time(at)),
        None => ", no event has arrived".to_owned(),
    });
    if let Some(at) = feed["last_error"]["at"].as_u64() {
        text.push_str(&format!(
            ", last error at {}: {}",
            time(at),
            feed["last_error"]["message"].as_str().unwrap_or_default()
        ));
    }
    text
}

/// The receiving end of the subscriptions; it stops when it is dropped.
pub struct Receiver {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Receiver {
    pub fn start(home: &Path, surroundings: Arc<dyn Surroundings>, feeds: Feeds) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (home, stop) = (home.to_path_buf(), Arc::clone(&stop));
            thread::spawn(move || run(&home, &*surroundings, &stop, &feeds))
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
    /// The ids handed on with their `created_at`, so an event the feed sends again is not
    /// handed on again. One older than the overlap is not asked for again, and is let go.
    seen: HashMap<String, u64>,
}

fn run(home: &Path, surroundings: &dyn Surroundings, stop: &AtomicBool, feeds: &Feeds) {
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
        feeds.forget_except(&kept);

        let secret = subscribe::receiving_secret(home);
        for entry in kept {
            if entry.exhausted() {
                feeds.enter(&entry.relay, "exhausted");
                continue;
            }
            if workers.contains_key(&entry.relay)
                || retries
                    .get(&entry.relay)
                    .is_some_and(|retry| Instant::now() < retry.after)
            {
                continue;
            }
            let Some(secret) = secret else {
                feeds.retrying(
                    &entry.relay,
                    "the subscriber key's secret is not kept for the supervisor",
                );
                continue;
            };
            let Some(relay_address) = surroundings.relay() else {
                feeds.retrying(&entry.relay, "the agent node's own relay is not running");
                continue;
            };
            let relay = entry.relay.clone();
            feeds.enter(&relay, "connecting");
            let feeds = feeds.clone();
            let filter = entry.filter.clone();
            let progress = Arc::clone(progress.entry(relay.clone()).or_default());
            let stopping = Arc::new(AtomicBool::new(false));
            let thread = {
                let (home, proxy) = (home.to_path_buf(), proxy_for(surroundings, &entry.relay));
                let stopping = Arc::clone(&stopping);
                thread::spawn(move || {
                    receive(
                        &home,
                        &entry,
                        &secret,
                        Route {
                            proxy,
                            own: relay_address,
                        },
                        &stopping,
                        &progress,
                        &feeds,
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

/// How a feed is reached and where its events go.
struct Route {
    /// The proxy the feed is dialled through, if any.
    proxy: Option<SocketAddr>,
    /// The write address of the agent node's own relay.
    own: SocketAddr,
}

/// Read one relay's feed until it ends, handing each event to the relay at `route.own`.
fn receive(
    home: &Path,
    entry: &Kept,
    secret: &[u8; 32],
    Route { proxy, own }: Route,
    stop: &AtomicBool,
    progress: &Mutex<Progress>,
    feeds: &Feeds,
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
    let ended = feed::read(
        &entry.relay,
        secret,
        &filter,
        proxy,
        stop,
        |event| {
            let mut progress = progress.lock().expect("the progress");
            let id = event["id"].as_str().unwrap_or_default().to_owned();
            if progress.seen.contains_key(&id) {
                return true;
            }
            // An event the relay did not take is not marked as seen, so it comes again.
            let handed = hand_over(own, &event);
            if let Err(why) = &handed {
                feeds.failed(&entry.relay, why);
            }
            if handed.is_ok() {
                feeds.accepted(&entry.relay);
                let created = event["created_at"].as_u64().unwrap_or_default();
                progress.seen.insert(id, created);
                progress.newest = progress.newest.max(Some(created));
                let from = progress.newest.unwrap_or_default().saturating_sub(OVERLAP);
                progress.seen.retain(|_, created| *created >= from);
            }
            true
        },
        || feeds.enter(&entry.relay, "live"),
    );
    match ended {
        Ended::Exhausted(why) => {
            let _ = subscribe::mark_exhausted(home, entry);
            feeds.failed(&entry.relay, &why);
            feeds.enter(&entry.relay, "exhausted");
        }
        Ended::Closed(why) => feeds.retrying(
            &entry.relay,
            &format!("{} closed the feed: {why}", entry.relay),
        ),
        Ended::Dropped(why) => feeds.retrying(&entry.relay, &why),
        Ended::Stopped => {}
    }
}

/// Write `event` to the relay at `own`: its write endpoint, or its ephemeral one for an
/// event that is not stored. Why the relay did not take it, if it did not.
fn hand_over(own: SocketAddr, event: &Value) -> Result<(), String> {
    let path = match event["kind"].as_u64() {
        Some(20000..=29999) => "/write-ephemeral",
        _ => "/write",
    };
    let body = event::write_body(event);
    let id = event["id"].as_str().unwrap_or_default();
    let mut stream = TcpStream::connect_timeout(&own, Duration::from_secs(5))
        .map_err(|error| format!("the own relay at {own} did not accept event {id}: {error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: relay\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut answer = String::new();
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.read_to_string(&mut answer).map(|_| ()))
        .map_err(|error| format!("the own relay at {own} did not answer event {id}: {error}"))?;
    let status = answer
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok());
    match status {
        Some(status) if (200..300).contains(&status) => Ok(()),
        _ => {
            let reply = answer.split("\r\n\r\n").nth(1).unwrap_or_default().trim();
            Err(format!(
                "the own relay refused event {id} (status {}): {reply}",
                status.map_or_else(|| "unknown".to_owned(), |status| status.to_string())
            ))
        }
    }
}
