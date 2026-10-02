//! How a request a `toon` command makes itself leaves this machine (ADR 0003).
//!
//! On a hidden agent node, one with at least one TOON app that is a hidden service, every
//! such request goes through the overlay's SOCKS5 proxy and names its host (`socks5h`), so
//! that nothing is resolved here and the operator's address is not tied to the settlement
//! address or the relays read. The one exception is the connector's own: a plain `http://`
//! or `ws://` endpoint on this machine (`overlay::is_local_plain`). If the overlay cannot
//! be had, the request fails with `overlay_unavailable` and nothing is dialled directly.
//! Anywhere else, requests are dialled directly.
//!
//! The overlay is bootstrapped by the first request that needs it, once per process, and
//! given up by `release` when the command is done.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::node::{Reach, State};
use crate::outcome::Error;
use crate::overlay::{self, Edge};

/// The overlay this process bootstrapped for a request, and the agent node it is of.
static EDGE: Mutex<Option<(PathBuf, Box<dyn Edge>)>> = Mutex::new(None);

/// Whether an agent node is hidden, and where it is.
#[derive(Clone, Debug)]
pub struct Egress {
    home: PathBuf,
    hidden: bool,
}

impl Egress {
    /// Requests are dialled directly: there is no agent node to hide.
    pub fn direct() -> Self {
        Self {
            home: PathBuf::new(),
            hidden: false,
        }
    }

    /// The egress of the agent node at `home`; direct if it has none.
    pub fn of(home: &Path) -> Result<Self, Error> {
        Ok(match State::load(home)? {
            Some(state) => Self::of_state(home, &state),
            None => Self::direct(),
        })
    }

    pub fn of_state(home: &Path, state: &State) -> Self {
        Self {
            home: home.to_path_buf(),
            hidden: state.toon_apps.iter().any(|app| app.reach == Reach::Hidden),
        }
    }

    /// The egress for a command that has no agent node of its own to name: the one on this
    /// machine, if there is one.
    pub fn open() -> Result<Self, Error> {
        match crate::home::resolve() {
            Ok(home) => Self::of(&home),
            Err(_) => Ok(Self::direct()),
        }
    }

    /// The overlay's proxy a request to `url` goes through, or `None` if it is dialled
    /// directly.
    pub fn proxy_for(&self, url: &str) -> Result<Option<SocketAddr>, Error> {
        if !self.hidden || overlay::is_local_plain(url) {
            return Ok(None);
        }
        let mut held = EDGE.lock().unwrap_or_else(|e| e.into_inner());
        if !held.as_ref().is_some_and(|(home, _)| *home == self.home) {
            let edge = overlay::bootstrap(&self.home, false)?;
            *held = Some((self.home.clone(), edge));
        }
        Ok(held.as_ref().map(|(_, edge)| edge.proxy()))
    }

    /// A blocking HTTP client for a request to `url`.
    pub fn client(&self, url: &str, timeout: Duration) -> Result<reqwest::blocking::Client, Error> {
        let mut builder = reqwest::blocking::Client::builder().timeout(timeout);
        if let Some(proxy) = self.proxy_for(url)? {
            builder = builder.proxy(
                reqwest::Proxy::all(format!("socks5h://{proxy}"))
                    .map_err(|error| self.unusable(error))?,
            );
        }
        builder.build().map_err(|error| self.unusable(error))
    }

    fn unusable(&self, error: reqwest::Error) -> Error {
        Error {
            code: crate::outcome::ErrorCode::OverlayUnavailable,
            message: format!("The overlay's proxy could not be used: {error}."),
        }
    }
}

/// The command is done: give up the overlay it bootstrapped, unless a supervisor runs.
pub fn release() {
    let held = EDGE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((home, edge)) = held {
        if !home.join("supervisor.sock").exists() {
            edge.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::overlay::is_local_plain;

    #[test]
    fn only_a_plain_endpoint_on_this_machine_skips_the_proxy() {
        for local in [
            "http://localhost:8545",
            "http://127.0.0.1:8545/rpc",
            "http://[::1]:8545",
            "ws://localhost:7100",
            "ws://127.0.0.1:7100/",
            "http://localhost",
        ] {
            assert!(is_local_plain(local), "{local}");
        }
        for remote in [
            "https://localhost:8545",
            "https://127.0.0.1:8545",
            "wss://localhost:7100",
            "http://example.com/rpc",
            "http://localhost.example.com:8545",
            "http://127.0.0.1@example.com/",
            "ws://abc.anyone:7100",
            "localhost:8545",
        ] {
            assert!(!is_local_plain(remote), "{remote}");
        }
    }
}
