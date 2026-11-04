//! Waiting for a thread for a limited time.

use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long a feed's thread gets to end once asked to: a receiver's worker can sit in a
/// socket read for its whole read timeout.
pub const FEED_GRACE: Duration = Duration::from_secs(10);

/// Wait for `thread` to end for `FEED_GRACE`, then leave it: it ends by itself, or with the
/// process.
pub fn join(thread: JoinHandle<()>) {
    let deadline = Instant::now() + FEED_GRACE;
    while !thread.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if thread.is_finished() {
        let _ = thread.join();
    }
}
