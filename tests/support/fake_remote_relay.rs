//! A relay that sells its live feed, written from `nips/paid-subscription.md`, for the
//! tests of the subscribe commands. No real relay implements the subscribe route yet.
//!
//! It serves, on one loopback port, the information document (with `toon_subscription`),
//! the subscribe route (a `POST` of `{filter?}` that its connector delivered, authorized
//! by NIP-98) and the read of a balance (a `GET` that accepts
//! `application/toon-subscription+json`). It keeps one balance per subscriber key.
//!
//! On the same port it serves the live feed as the draft says: an `AUTH` challenge when a
//! websocket opens, the stored events that match a `REQ` and `EOSE`, and then each event the
//! test hands to `broadcast`, debited at the broadcast price from the subscription of the
//! key the connection authenticated with. When a balance is below the price the `REQ` is
//! closed with `payment-required`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use k256::schnorr::{Signature, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    pub balance: u64,
    pub filter: Value,
}

#[derive(Default)]
struct State {
    connector_url: String,
    subscriptions: HashMap<String, Subscription>,
    /// The subscriber key of every request the subscribe route accepted, in order.
    credited: Vec<String>,
    /// Every `POST` the subscribe route was delivered, accepted or not.
    posts: usize,
    /// Every event the relay accepted, in order: stored, and broadcast to the feeds.
    events: Vec<Value>,
    /// How many `REQ`s are open on a connection that holds a subscription.
    open_feeds: usize,
    /// How many `REQ`s were opened and closed with `payment-required`.
    refused_feeds: usize,
}

pub struct FakeRemoteRelay {
    address: SocketAddr,
    price: u64,
    broadcast_price: u64,
    ilp_address: String,
    state: Arc<Mutex<State>>,
    /// The other authorities the relay is reached at, as a client names it in a NIP-98 `u`.
    aliases: Arc<Mutex<Vec<String>>>,
}

impl FakeRemoteRelay {
    /// Serve a relay whose subscribe route is `ilp_address`, charging `price` a packet and
    /// debiting `broadcast_price` an event.
    pub fn start(ilp_address: &str, price: u64, broadcast_price: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let relay = Self {
            address: listener.local_addr().expect("address"),
            price,
            broadcast_price,
            ilp_address: ilp_address.to_owned(),
            state: Arc::default(),
            aliases: Arc::default(),
        };
        let served = relay.shared();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = served.shared();
                thread::spawn(move || served.serve(stream));
            }
        });
        relay
    }

    fn shared(&self) -> Self {
        Self {
            address: self.address,
            price: self.price,
            broadcast_price: self.broadcast_price,
            ilp_address: self.ilp_address.clone(),
            state: self.state.clone(),
            aliases: self.aliases.clone(),
        }
    }

    /// The relay's `ws://` URL.
    pub fn url(&self) -> String {
        format!("ws://{}", self.address)
    }

    /// The URL of the relay's subscribe route, for `toon add --url`.
    pub fn app_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Say that the relay is also reached at `authority`, a `host:port` a client names its
    /// requests with: the overlay's proxy takes a client there.
    pub fn also_at(&self, authority: &str) {
        self.aliases.lock().unwrap().push(authority.to_owned());
    }

    /// Name the connector that terminates the relay's routes.
    pub fn set_connector(&self, url: &str) {
        self.state.lock().unwrap().connector_url = url.to_owned();
    }

    pub fn subscription(&self, pubkey: &str) -> Option<Subscription> {
        self.state
            .lock()
            .unwrap()
            .subscriptions
            .get(pubkey)
            .cloned()
    }

    /// Forget the subscription of `pubkey`, as a relay may forget an exhausted one.
    pub fn forget(&self, pubkey: &str) {
        self.state.lock().unwrap().subscriptions.remove(pubkey);
    }

    /// The subscriber key of each request credited, in order.
    pub fn credited(&self) -> Vec<String> {
        self.state.lock().unwrap().credited.clone()
    }

    pub fn posts(&self) -> usize {
        self.state.lock().unwrap().posts
    }

    /// Accept `event`: store it, and send it on every open `REQ` it is owed to.
    pub fn broadcast(&self, event: Value) {
        self.state.lock().unwrap().events.push(event);
    }

    /// How many `REQ`s are open on a connection holding a subscription that has a balance.
    pub fn open_feeds(&self) -> usize {
        self.state.lock().unwrap().open_feeds
    }

    /// How many `REQ`s were closed with `payment-required`.
    pub fn refused_feeds(&self) -> usize {
        self.state.lock().unwrap().refused_feeds
    }

    fn document(&self) -> Value {
        json!({
            "name": "far",
            "supported_nips": [1, 11, 42],
            "toon": {
                "ilp_address": "g.toon.relay",
                "connector_url": self.state.lock().unwrap().connector_url,
                "price": 1,
            },
            "toon_subscription": {
                "ilp_address": self.ilp_address,
                "price": self.price,
                "broadcast_price": self.broadcast_price,
            },
        })
    }

    fn serve(&self, stream: TcpStream) {
        let mut start = [0u8; 1024];
        let peeked = stream.peek(&mut start).unwrap_or(0);
        if String::from_utf8_lossy(&start[..peeked])
            .to_ascii_lowercase()
            .contains("upgrade: websocket")
        {
            return self.feed(stream);
        }
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        if reader.read_line(&mut request).is_err() {
            return;
        }
        let method = request
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length = headers
            .get("content-length")
            .and_then(|length| length.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);

        let accepts = headers.get("accept").cloned().unwrap_or_default();
        let (status, content_type, answer) = match method.as_str() {
            "POST" => {
                let (status, answer) = self.subscribe(&headers, &body);
                (status, "application/json", answer)
            }
            "GET" if accepts.contains("application/toon-subscription+json") => {
                let (status, answer) = self.balance(&headers);
                (status, "application/toon-subscription+json", answer)
            }
            _ => (200, "application/nostr+json", self.document()),
        };
        let answer = answer.to_string();
        let _ = write!(
            reader.get_mut(),
            "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
            answer.len()
        );
    }

    /// The public key of `event`, if its id and signature hold.
    fn verified(event: &Value) -> Option<String> {
        let pubkey = event["pubkey"].as_str()?;
        let id = Sha256::digest(
            json!([
                0,
                pubkey,
                event["created_at"],
                event["kind"],
                event["tags"],
                event["content"]
            ])
            .to_string(),
        );
        let verifies = VerifyingKey::from_bytes(&hex::decode(pubkey).ok()?)
            .ok()?
            .verify_raw(
                &id,
                &Signature::try_from(hex::decode(event["sig"].as_str()?).ok()?.as_slice()).ok()?,
            )
            .is_ok();
        (verifies && hex::encode(id) == event["id"].as_str()?).then(|| pubkey.to_owned())
    }

    fn tag(event: &Value, name: &str) -> Option<String> {
        event["tags"]
            .as_array()?
            .iter()
            .find_map(|tag| (tag[0] == name).then(|| tag[1].as_str().map(str::to_owned))?)
    }

    /// The subscriber key a NIP-98 authorization names, if the authorization holds.
    fn authorized(
        &self,
        headers: &HashMap<String, String>,
        method: &str,
        body: Option<&[u8]>,
    ) -> Option<String> {
        let encoded = headers.get("authorization")?.strip_prefix("Nostr ")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let event: Value = serde_json::from_slice(&bytes).ok()?;
        let pubkey = Self::verified(&event)?;
        let created_at = event["created_at"].as_u64()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let payload = body.map(|body| hex::encode(Sha256::digest(body)));
        let mut this_relay = vec![
            format!("http://{}", self.address),
            format!("http://{}/", self.address),
        ];
        for alias in self.aliases.lock().unwrap().iter() {
            this_relay.push(format!("http://{alias}"));
            this_relay.push(format!("http://{alias}/"));
        }
        (event["kind"] == 27235
            && Self::tag(&event, "method").as_deref() == Some(method)
            && Self::tag(&event, "u").is_some_and(|u| this_relay.contains(&u))
            && Self::tag(&event, "payload") == payload
            && created_at.abs_diff(now) <= 60)
            .then_some(pubkey)
    }

    fn refusal(status: u16, code: &str, message: &str) -> (u16, Value) {
        (
            status,
            json!({ "error": { "code": code, "message": message } }),
        )
    }

    fn subscribe(&self, headers: &HashMap<String, String>, body: &[u8]) -> (u16, Value) {
        let mut state = self.state.lock().unwrap();
        state.posts += 1;
        let Some(pubkey) = self.authorized(headers, "POST", Some(body)) else {
            return Self::refusal(401, "unauthorized", "the authorization does not hold");
        };
        let request: Option<Value> = serde_json::from_slice(body).ok().filter(Value::is_object);
        let Some(request) =
            request.filter(|request| request.get("filter").is_none_or(Value::is_object))
        else {
            return Self::refusal(400, "invalid_request", "not a subscribe request");
        };
        if request.get("filter").is_none() && !state.subscriptions.contains_key(&pubkey) {
            return Self::refusal(400, "filter_required", "a first payment needs a filter");
        }
        // What the route charged: the connector states it, else the route's price.
        let credited = headers
            .get("x-toon-amount")
            .and_then(|amount| amount.parse().ok())
            .unwrap_or(self.price);
        let subscription = state
            .subscriptions
            .entry(pubkey.clone())
            .or_insert_with(|| Subscription {
                balance: 0,
                filter: Value::Null,
            });
        subscription.balance += credited;
        if let Some(filter) = request.get("filter") {
            subscription.filter = filter.clone();
        }
        let answer = json!({
            "pubkey": pubkey,
            "credited": credited,
            "balance": subscription.balance,
            "broadcast_price": self.broadcast_price,
            "filter": subscription.filter,
        });
        state.credited.push(pubkey);
        (200, answer)
    }

    fn balance(&self, headers: &HashMap<String, String>) -> (u16, Value) {
        let Some(pubkey) = self.authorized(headers, "GET", None) else {
            return Self::refusal(401, "unauthorized", "the authorization does not hold");
        };
        match self.state.lock().unwrap().subscriptions.get(&pubkey) {
            Some(subscription) => (
                200,
                json!({
                    "pubkey": pubkey,
                    "balance": subscription.balance,
                    "broadcast_price": self.broadcast_price,
                    "filter": subscription.filter,
                }),
            ),
            None => Self::refusal(404, "not_subscribed", "the key has no subscription"),
        }
    }
}

/// Whether `event` is among what `filter` asks for: `ids`, `authors`, `kinds` and `since`.
fn matches(filter: &Value, event: &Value) -> bool {
    let listed = |wanted: &str, field: &str| {
        filter[wanted]
            .as_array()
            .is_none_or(|allowed| allowed.contains(&event[field]))
    };
    listed("ids", "id")
        && listed("authors", "pubkey")
        && listed("kinds", "kind")
        && filter["since"]
            .as_u64()
            .is_none_or(|since| event["created_at"].as_u64().unwrap_or(0) >= since)
}

impl FakeRemoteRelay {
    /// One websocket: the live feed.
    fn feed(&self, stream: TcpStream) {
        let Ok(mut socket) = tungstenite::accept(stream) else {
            return;
        };
        let _ = socket
            .get_ref()
            .set_read_timeout(Some(std::time::Duration::from_millis(20)));
        let challenge = hex::encode(Sha256::digest(format!("{:?}", SystemTime::now())));
        let say = |socket: &mut tungstenite::WebSocket<TcpStream>, frame: Value| {
            let _ = socket.send(tungstenite::Message::text(frame.to_string()));
        };
        say(&mut socket, json!(["AUTH", challenge]));

        let mut key: Option<String> = None;
        // The `REQ` that is open, and how many events the relay had accepted when it
        // sent `EOSE`: the live feed is what comes after.
        let mut open: Option<(Value, Vec<Value>, usize)> = None;
        let mut counted = false;
        loop {
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    let Ok(Value::Array(frame)) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    match frame.first().and_then(Value::as_str) {
                        Some("AUTH") => {
                            let event = frame.get(1).cloned().unwrap_or_default();
                            let proves = Self::verified(&event).filter(|_| {
                                event["kind"] == 22242
                                    && Self::tag(&event, "challenge").as_deref()
                                        == Some(challenge.as_str())
                            });
                            let accepted = proves.is_some();
                            if proves.is_some() {
                                key = proves;
                            }
                            say(
                                &mut socket,
                                json!([
                                    "OK",
                                    event["id"],
                                    accepted,
                                    if accepted { "" } else { "invalid: auth" }
                                ]),
                            );
                        }
                        Some("REQ") if frame.len() >= 3 => {
                            let id = frame[1].clone();
                            let filters = frame[2..].to_vec();
                            let stored: Vec<Value> = self.state.lock().unwrap().events.clone();
                            for event in stored
                                .iter()
                                .filter(|event| filters.iter().any(|f| matches(f, event)))
                            {
                                say(&mut socket, json!(["EVENT", id, event]));
                            }
                            say(&mut socket, json!(["EOSE", id]));
                            let held = key.as_ref().and_then(|key| self.subscription(key));
                            match (&key, held) {
                                (None, _) => say(
                                    &mut socket,
                                    json!([
                                        "CLOSED",
                                        id,
                                        "auth-required: the live feed is for subscribers"
                                    ]),
                                ),
                                (Some(_), Some(held)) if held.balance >= self.broadcast_price => {
                                    open = Some((id, filters, stored.len()));
                                    if !counted {
                                        counted = true;
                                        self.state.lock().unwrap().open_feeds += 1;
                                    }
                                }
                                (Some(_), _) => {
                                    self.state.lock().unwrap().refused_feeds += 1;
                                    say(
                                        &mut socket,
                                        json!(["CLOSED", id, "payment-required: the subscription's balance has run out"]),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(tungstenite::Message::Close(_)) => break,
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
            // Events accepted since the last look, owed to the open `REQ`.
            if let (Some((id, filters, next)), Some(key)) = (open.as_mut(), key.as_ref()) {
                let accepted: Vec<Value> = self.state.lock().unwrap().events.clone();
                while *next < accepted.len() {
                    let event = &accepted[*next];
                    *next += 1;
                    let mut state = self.state.lock().unwrap();
                    let Some(subscription) = state.subscriptions.get_mut(key) else {
                        continue;
                    };
                    let owed = subscription.balance >= self.broadcast_price
                        && matches(&subscription.filter, event)
                        && filters.iter().any(|filter| matches(filter, event));
                    if !owed {
                        continue;
                    }
                    subscription.balance -= self.broadcast_price;
                    let ran_out = subscription.balance < self.broadcast_price;
                    if ran_out {
                        state.refused_feeds += 1;
                    }
                    drop(state);
                    say(&mut socket, json!(["EVENT", id, event]));
                    if ran_out {
                        say(
                            &mut socket,
                            json!([
                                "CLOSED",
                                id,
                                "payment-required: the subscription's balance has run out"
                            ]),
                        );
                        break;
                    }
                }
                if self
                    .subscription(key)
                    .is_none_or(|held| held.balance < self.broadcast_price)
                {
                    open = None;
                }
            }
            if open.is_none() && counted {
                counted = false;
                let mut state = self.state.lock().unwrap();
                state.open_feeds = state.open_feeds.saturating_sub(1);
            }
        }
        if counted {
            let mut state = self.state.lock().unwrap();
            state.open_feeds = state.open_feeds.saturating_sub(1);
        }
    }
}
