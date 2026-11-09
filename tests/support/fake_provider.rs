//! A TOON Compute provider's app, written from §5 and §6 of the spec and the wire fixtures,
//! for the tests of `toon compute`. Each route of the provider is served by its own connector
//! route, whose handler URL is `<provider>/<path>`: `availability`, `status`, `terminate`,
//! `rotate` and `listings/<name>/v<n>/spawn`. It keeps what it was sent, so a test can say what reached it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

#[derive(Default)]
pub struct State {
    /// The path and JSON body of every request, in order.
    pub requests: Vec<(String, Value)>,
    /// What `availability` answers: `would_run: true` unless this names a refusal.
    pub refuse_availability: Option<String>,
    /// A spawn that is answered with this error instead of a lease.
    pub spawn_error: Option<String>,
    /// A directory to look for the tenant's lease file in when a spawn arrives, and what was
    /// found: whether the file was there, and its mode.
    pub lease_dir: Option<PathBuf>,
    pub file_at_spawn: Option<(bool, u32)>,
    /// The `next_secret` the lease file in `lease_dir` held when the first rotate arrived.
    pub next_secret_at_rotate: Option<Option<String>>,
    /// A rotate that is answered `unavailable`, changing nothing.
    pub rotate_unavailable: bool,
    /// A rotate that takes effect but whose answer is an `unavailable` all the same, as if
    /// the confirmation were lost.
    pub rotate_answer_lost: bool,
    /// The nth `extend` (counting from 1) is answered with an error instead of an extension.
    pub fail_extend_at: Option<usize>,
    /// A spawn (or standby) whose connection is closed with no answer, as by a provider that
    /// crashed.
    pub drop_spawn: bool,
    /// The nth `extend` (counting from 1) and every one after it is applied, but its
    /// connection is closed with no answer.
    pub drop_extend_from: Option<usize>,
    extends: usize,
    token: Option<String>,
    /// The lease it holds; `None` answers every route `unknown_workload`, as a provider
    /// that never ran the spawn does.
    pub workload: Option<String>,
    /// `standalone`, or `standby` for a reservation made on `.standby`.
    role: String,
    ended: bool,
    expires_at: u64,
}

#[derive(Clone)]
pub struct FakeProvider {
    address: SocketAddr,
    pubkey: String,
    state: Arc<Mutex<State>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn error(code: &str, message: &str) -> Value {
    json!({ "error": code, "message": message })
}

impl FakeProvider {
    /// A provider whose public key is `pubkey`.
    pub fn start(pubkey: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let provider = Self {
            address: listener.local_addr().expect("address"),
            pubkey: pubkey.to_owned(),
            state: Arc::default(),
        };
        let served = provider.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let served = served.clone();
                thread::spawn(move || served.serve(stream));
            }
        });
        provider
    }

    /// The handler URL of the route at `path`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn with<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().unwrap())
    }

    /// How many requests reached the route that ends in `route`.
    pub fn count(&self, route: &str) -> usize {
        self.with(|state| {
            state
                .requests
                .iter()
                .filter(|(path, _)| path.trim_end_matches('/').ends_with(route))
                .count()
        })
    }

    /// The body of the last request to the route that ends in `route`.
    pub fn last(&self, route: &str) -> Option<Value> {
        self.with(|state| {
            state
                .requests
                .iter()
                .rev()
                .find(|(path, _)| path.trim_end_matches('/').ends_with(route))
                .map(|(_, body)| body.clone())
        })
    }

    /// When the lease the provider holds now ends.
    pub fn expires_at(&self) -> u64 {
        self.with(|state| state.expires_at)
    }

    fn serve(&self, mut stream: TcpStream) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let (head, mut body) = loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(at) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break (
                    String::from_utf8_lossy(&buffer[..at]).into_owned(),
                    buffer[at + 4..].to_vec(),
                );
            }
        };
        let path = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        while body.len() < length {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let route = path.trim_end_matches('/');
        let dropped = self.with(|state| {
            state.drop_spawn && (route.ends_with("/spawn") || route.ends_with("/standby"))
        });
        let answer = self.answer(&path, &parsed);
        let dropped = dropped
            || self.with(|state| {
                route.ends_with("/extend")
                    && state
                        .drop_extend_from
                        .is_some_and(|from| state.extends >= from)
            });
        if dropped {
            return;
        }
        let text = answer.to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{text}",
            text.len()
        );
    }

    fn answer(&self, path: &str, body: &Value) -> Value {
        let mut state = self.state.lock().unwrap();
        state.requests.push((path.to_owned(), body.clone()));
        let route = path.trim_end_matches('/');
        if route.ends_with("/availability") {
            return match &state.refuse_availability {
                Some(code) => json!({ "would_run": false, "error": code, "message": "refused" }),
                None => json!({ "would_run": true }),
            };
        }
        // `extend` is the one paid route with a bare body: `{ "workload_id" }` and nothing else.
        if route.ends_with("/extend") {
            let standby_route = route.ends_with("/standby/extend");
            let Some(workload) = body["workload_id"]
                .as_str()
                .filter(|_| body.as_object().is_some_and(|object| object.len() == 1))
            else {
                return error("invalid_request", "not the bare extend body");
            };
            state.extends += 1;
            if state.fail_extend_at == Some(state.extends) {
                return error("unavailable", "the extension failed");
            }
            if state.workload.as_deref() != Some(workload) {
                return error("unknown_workload", "no such lease");
            }
            if standby_route != (state.role == "standby") {
                return if standby_route {
                    error("not_standby", "this lease is not a Warm Standby")
                } else {
                    error(
                        "not_running",
                        "this lease is a reservation; extend it on .standby.extend",
                    )
                };
            }
            state.expires_at += 3600;
            return json!({ "workload_id": workload, "expires_at": state.expires_at });
        }
        // Every other route carries a Lease Request, and nothing else (spec §5).
        let Some(request) = body["request"]
            .as_object()
            .filter(|_| body.as_object().is_some_and(|object| object.len() == 1))
        else {
            return error("invalid_request", "a bare body");
        };
        let field = |name: &str| request[name].as_str().unwrap_or_default().to_owned();
        let op = if route.ends_with("/spawn") {
            "spawn"
        } else if route.ends_with("/standby") {
            "standby"
        } else if route.ends_with("/status") {
            "status"
        } else if route.ends_with("/terminate") {
            "terminate"
        } else if route.ends_with("/rotate") {
            "rotate"
        } else {
            return error("invalid_request", "unknown route");
        };
        if field("op") != op || field("provider") != self.pubkey || request["content"].is_null() {
            return error("invalid_request", "wrong op or provider");
        }
        let workload = request["content"]["workload_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if op == "spawn" || op == "standby" {
            if let Some(code) = &state.spawn_error {
                return error(code, "the spawn was refused");
            }
            if let Some(dir) = state.lease_dir.clone() {
                let file = dir.join(format!("{workload}.json"));
                let mode = std::fs::metadata(&file)
                    .map(|meta| meta.permissions().mode() & 0o777)
                    .unwrap_or(0);
                state.file_at_spawn = Some((file.exists(), mode));
            }
            state.token = Some(field("continuation"));
            state.workload = Some(workload.clone());
            state.expires_at = unix_now() + 3600;
            if op == "standby" {
                state.role = "standby".to_owned();
                return json!({
                    "workload_id": workload,
                    "role": "standby",
                    "expires_at": state.expires_at,
                });
            }
            state.role = if request["content"]["standby_set"].is_array() {
                "primary".to_owned()
            } else {
                "standalone".to_owned()
            };
            return json!({
                "workload_id": workload,
                "role": state.role,
                "expires_at": state.expires_at,
                "access": {
                    "host": "203.0.113.7",
                    "ssh_port": 40000,
                    "ports": [{ "container_port": 443, "host_port": 41000 }],
                },
            });
        }
        if state.workload.as_deref() != Some(workload.as_str()) {
            return error("unknown_workload", "no such lease");
        }
        if state.token.as_deref() != Some(field("continuation").as_str()) {
            return error("not_tenant", "not the tenant");
        }
        if state.ended {
            return error("expired", "the lease has ended");
        }
        if op == "rotate" {
            if let (Some(dir), None) = (state.lease_dir.clone(), &state.next_secret_at_rotate) {
                let file = std::fs::read_to_string(dir.join(format!("{workload}.json")))
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok());
                state.next_secret_at_rotate =
                    Some(file.and_then(|file| file["next_secret"].as_str().map(str::to_owned)));
            }
            if state.rotate_unavailable {
                return error("unavailable", "could not save that right now; try again");
            }
            let next = request["content"]["next"].as_str().unwrap_or_default();
            state.token = Some(next.to_owned());
            if state.rotate_answer_lost {
                return error("unavailable", "could not save that right now; try again");
            }
            return json!({ "workload_id": workload, "rotated": true });
        }
        if op == "terminate" {
            state.ended = true;
            return json!({ "workload_id": workload, "state": { "ended": "termination" } });
        }
        if state.role == "standby" {
            return json!({
                "workload_id": workload,
                "role": "standby",
                "state": "reserved",
                "expires_at": state.expires_at,
            });
        }
        json!({
            "workload_id": workload,
            "role": state.role,
            "state": "running",
            "expires_at": state.expires_at,
            "access": { "host": "203.0.113.7", "ssh_port": 40000, "ports": [] },
        })
    }
}
