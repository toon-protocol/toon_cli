//! A listener that counts the connections made to it: a published `connector_url` that
//! a test can show was never dialled.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub struct Spy {
    address: String,
    seen: Arc<AtomicUsize>,
}

pub fn start() -> Spy {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address").to_string();
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = seen.clone();
    thread::spawn(move || {
        for _ in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    Spy { address, seen }
}

impl Spy {
    /// The URL to publish, in the shape of a connector's.
    pub fn url(&self) -> String {
        format!("http://{}/ilp", self.address)
    }

    /// How many connections were made to the spy before now. It connects once itself and
    /// waits to count that, so a connection still waiting to be accepted is counted too.
    pub fn connections(&self) -> usize {
        let _sentinel = TcpStream::connect(&self.address).expect("connect to the spy");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = self.seen.load(Ordering::SeqCst);
        while seen == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
            seen = self.seen.load(Ordering::SeqCst);
        }
        assert!(seen > 0, "the spy did not see its own connection");
        seen - 1
    }
}
