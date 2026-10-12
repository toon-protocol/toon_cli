//! Orders (ADR 0010): requests to the supervisor signed with an agent key, as a write is a
//! signed request to a connector.
//!
//! The supervisor listens for them on loopback TCP, beside its local socket. Every order goes
//! through `Orders::authorise`, the one place that decides whether a request may be
//! answered: it checks the RFC 9421 signature the way a connector checks a write, looks the
//! key up in the home's record of agent keys on every order (so a revoke takes effect on the
//! next one), refuses a replayed signature, and checks the key's scope. An order never moves
//! money.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::agent_key::{Key, Record};

/// The orders the supervisor answers, by name (`/orders/<name>`), with the scope each one
/// needs. `sign` and `seal` both sign as the agent identity, so both need `sign`.
const ORDERS: [(&str, &str); 8] = [
    ("status", "status"),
    ("logs", "logs"),
    ("add", "apps"),
    ("remove", "apps"),
    ("route-price", "route-price"),
    ("sign", "sign"),
    ("seal", "sign"),
    ("messages", "read-messages"),
];

/// The components a signature covers: exactly these, as a connector's write signature does.
const COVERED: [&str; 3] = ["@method", "@path", "content-digest"];

/// How often the listener looks at the record for an address it has not bound yet.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// What answers an authorised order.
type Answer = dyn Fn(&Key, Order) -> Result<Value, Refusal> + Send + Sync;

/// An order that has been authorised: what it is and what it carries.
pub struct Order {
    pub name: &'static str,
    pub body: Value,
}

/// Why an order was refused, and how the answer says so.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    status: u16,
    code: &'static str,
    message: String,
    /// The scope the key lacks, when that is the reason.
    scope: Option<String>,
}

impl Refusal {
    fn unauthorised(code: &'static str, message: &str) -> Self {
        Self {
            status: 401,
            code,
            message: message.to_owned(),
            scope: None,
        }
    }

    /// An authorised order that cannot be carried out as it stands.
    pub fn bad_order(code: &'static str, message: String) -> Self {
        Self {
            status: 400,
            code,
            message,
            scope: None,
        }
    }

    fn forbidden(code: &'static str, message: String) -> Self {
        Self {
            status: 403,
            code,
            message,
            scope: None,
        }
    }

    /// An authorised order that was carried out as far as it was allowed to: `error` is why
    /// it went no further.
    pub fn of_error(error: &crate::outcome::Error) -> Self {
        use crate::outcome::ErrorCode;
        let status = match error.code {
            ErrorCode::RateLimited => 429,
            ErrorCode::UnknownName | ErrorCode::Usage | ErrorCode::ConfirmationRequired => 400,
            _ => 500,
        };
        Self {
            status,
            code: error.code.as_str(),
            message: error.message.clone(),
            scope: None,
        }
    }

    fn json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(scope) = &self.scope {
            error["scope"] = json!(scope);
        }
        json!({ "error": error })
    }

    fn response(&self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::FORBIDDEN);
        (status, Json(self.json())).into_response()
    }
}

/// The three headers of a signed request.
struct Signed<'a> {
    input: Option<&'a str>,
    signature: Option<&'a str>,
    digest: Option<&'a str>,
}

/// What a `Signature-Input` header says (RFC 9421 §2.3), in the one form a write is signed in.
struct Params {
    created: u64,
    expires: u64,
    keyid: String,
}

fn text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_owned)
}

/// `sig1=("@method" "@path" "content-digest");created=1;expires=2;keyid="ab";alg="ed25519"`
/// as its label and parameters, or `None` for anything else.
fn parse_input(header: &str) -> Option<(String, Params)> {
    let (label, rest) = header.trim().split_once('=')?;
    let rest = rest.strip_prefix('(')?;
    let (components, rest) = rest.split_once(')')?;
    let covered: Vec<&str> = components
        .split_whitespace()
        .map(|item| item.strip_prefix('"')?.strip_suffix('"'))
        .collect::<Option<_>>()?;
    if covered != COVERED {
        return None;
    }
    let (mut created, mut expires, mut keyid, mut alg) = (None, None, None, None);
    for parameter in rest.strip_prefix(';')?.split(';') {
        let (name, value) = parameter.split_once('=')?;
        let quoted = || Some(value.strip_prefix('"')?.strip_suffix('"')?.to_owned());
        match name.trim() {
            "created" => created = Some(value.parse().ok()?),
            "expires" => expires = Some(value.parse().ok()?),
            "keyid" => keyid = Some(quoted()?),
            "alg" => alg = Some(quoted()?),
            _ => return None,
        }
    }
    if alg.is_some_and(|alg| alg != "ed25519") {
        return None;
    }
    Some((
        label.trim().to_owned(),
        Params {
            created: created?,
            expires: expires?,
            keyid: keyid?,
        },
    ))
}

/// `label=:base64:` as the signature's bytes, for the label `want`.
fn parse_signature(header: &str, want: &str) -> Option<Vec<u8>> {
    let (label, value) = header.trim().split_once('=')?;
    if label.trim() != want {
        return None;
    }
    let inner = value.trim().strip_prefix(':')?.strip_suffix(':')?;
    BASE64.decode(inner).ok()
}

fn signature_base(method: &str, path: &str, digest: &str, params: &Params) -> String {
    format!(
        "\"@method\": {}\n\"@path\": {path}\n\"content-digest\": {}\n\"@signature-params\": \
         (\"@method\" \"@path\" \"content-digest\");created={};expires={};keyid=\"{}\";\
         alg=\"ed25519\"",
        method.to_uppercase(),
        digest.trim(),
        params.created,
        params.expires,
        params.keyid
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// What the supervisor knows of the orders it has answered: the signatures it has seen.
pub struct Orders {
    home: PathBuf,
    /// Each signature that authorised an order, until it expires.
    seen: Mutex<HashMap<Vec<u8>, u64>>,
}

impl Orders {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// The key that signed the request, if the request may be answered; the reason it may
    /// not otherwise. Every order is decided here and nowhere else.
    fn authorise(
        &self,
        method: &str,
        path: &str,
        signed: &Signed,
        body: &[u8],
        scope: &str,
        now: u64,
    ) -> Result<Key, Refusal> {
        let (Some(input), Some(signature)) = (signed.input, signed.signature) else {
            return Err(Refusal::unauthorised(
                "unsigned",
                "An order is signed: it needs Signature-Input and Signature headers.",
            ));
        };
        let malformed = |what: &str| Refusal::unauthorised("malformed_signature", what);
        let (label, params) = parse_input(input).ok_or_else(|| {
            malformed(
                "Signature-Input must cover exactly @method, @path and content-digest, with \
                 created, expires and keyid.",
            )
        })?;
        let bytes = parse_signature(signature, &label)
            .ok_or_else(|| malformed("Signature is not the signature of that Signature-Input."))?;
        let digest = signed
            .digest
            .ok_or_else(|| malformed("An order carries a Content-Digest."))?;
        let expected = format!("sha-256=:{}:", BASE64.encode(Sha256::digest(body)));
        if digest.trim() != expected {
            return Err(malformed("Content-Digest does not match the body."));
        }
        if now > params.expires {
            return Err(Refusal::unauthorised(
                "expired",
                "The signature has expired.",
            ));
        }

        let record = Record::load(&self.home).map_err(|error| Refusal {
            status: 500,
            code: "record_unreadable",
            message: error.message,
            scope: None,
        })?;
        let unknown = || {
            Refusal::unauthorised(
                "unknown_key",
                "That key is not an agent key of this agent node.",
            )
        };
        let key = record
            .keys
            .iter()
            .find(|key| key.public == params.keyid)
            .ok_or_else(unknown)?;
        let public: [u8; 32] = hex::decode(&params.keyid)
            .ok()
            .and_then(|raw| raw.try_into().ok())
            .ok_or_else(unknown)?;
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&public).map_err(|_| unknown())?;
        let signature = ed25519_dalek::Signature::from_slice(&bytes).map_err(|_| {
            Refusal::unauthorised("bad_signature", "The signature does not verify.")
        })?;
        verifying
            .verify_strict(
                signature_base(method, path, digest, &params).as_bytes(),
                &signature,
            )
            .map_err(|_| {
                Refusal::unauthorised("bad_signature", "The signature does not verify.")
            })?;

        // A signature that authorised an order is remembered until it expires; the same one
        // again is a replay, whatever the key may now do. A refused order does not spend its
        // signature, as a refused write does not at a connector.
        let mut seen = self.seen.lock().expect("the signatures seen");
        seen.retain(|_, expires| *expires >= now);
        if seen.contains_key(&bytes) {
            return Err(Refusal::unauthorised(
                "replayed",
                "That signature has already been used.",
            ));
        }

        if let Some(expires) = &key.expires {
            let over = chrono::DateTime::parse_from_rfc3339(expires).map_or(true, |expires| {
                i64::try_from(now).unwrap_or(i64::MAX) >= expires.timestamp()
            });
            if over {
                return Err(Refusal::forbidden(
                    "key_expired",
                    "This agent key has expired.".to_owned(),
                ));
            }
        }
        if !key.scope.iter().any(|granted| granted == scope) {
            return Err(Refusal {
                status: 403,
                code: "out_of_scope",
                message: format!("This agent key's scope does not include '{scope}'."),
                scope: Some(scope.to_owned()),
            });
        }
        seen.insert(bytes, params.expires);
        Ok(key.clone())
    }

    fn handle(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
        answer: &Answer,
    ) -> Response {
        let path = uri.path();
        let Some((name, scope)) = ORDERS
            .iter()
            .find(|(name, _)| path.strip_prefix("/orders/") == Some(name))
            .copied()
        else {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": { "code": "usage", "message": "No such order." } })),
            )
                .into_response();
        };
        let (input, signature, digest) = (
            text(headers, "signature-input"),
            text(headers, "signature"),
            text(headers, "content-digest"),
        );
        let signed = Signed {
            input: input.as_deref(),
            signature: signature.as_deref(),
            digest: digest.as_deref(),
        };
        let key = match self.authorise(method.as_str(), path, &signed, body, scope, now()) {
            Ok(key) => key,
            Err(refusal) => return refusal.response(),
        };
        let body: Value = if body.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice(body) {
                Ok(body) => body,
                Err(_) => {
                    return Refusal::bad_order("usage", "An order's body is JSON.".to_owned())
                        .response()
                }
            }
        };
        match answer(&key, Order { name, body }) {
            Ok(answer) => Json(answer).into_response(),
            Err(refusal) => refusal.response(),
        }
    }
}

/// Whether `address` is somewhere only this machine can reach.
fn loopback(address: &SocketAddr) -> bool {
    address.ip().is_loopback()
}

/// The address `record` says orders are heard at, if it is one and is loopback. Anything else
/// is never bound (ADR 0003).
fn address_of(record: &Record) -> Option<SocketAddr> {
    let text = record.supervisor.as_deref()?;
    let address: SocketAddr = text.strip_prefix("http://")?.parse().ok()?;
    loopback(&address).then_some(address)
}

/// Listen for orders at the address the home's record of agent keys names, once it names one
/// (`toon agent-key create` writes it), and answer each authorised order with `answer`.
/// Returns at once; the listener runs on a thread of its own.
pub fn serve(
    home: &Path,
    answer: impl Fn(&Key, Order) -> Result<Value, Refusal> + Send + Sync + 'static,
) {
    let home = home.to_path_buf();
    let answer: Arc<Answer> = Arc::new(answer);
    thread::spawn(move || {
        let orders = Arc::new(Orders::new(&home));
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return;
        };
        let listener = loop {
            if let Some(address) = Record::load(&home).ok().as_ref().and_then(address_of) {
                if let Ok(listener) = TcpListener::bind(address) {
                    break listener;
                }
            }
            thread::sleep(WATCH_EVERY);
        };
        let _ = listener.set_nonblocking(true);
        let router = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let (orders, answer) = (Arc::clone(&orders), Arc::clone(&answer));
                async move {
                    tokio::task::spawn_blocking(move || {
                        orders.handle(&method, &uri, &headers, &body, answer.as_ref())
                    })
                    .await
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
                }
            },
        );
        runtime.block_on(async move {
            if let Ok(server) = axum::Server::from_tcp(listener) {
                let _ = server.serve(router.into_make_service()).await;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_is_bound() {
        let record = |address: &str| Record {
            supervisor: Some(address.to_owned()),
            keys: Vec::new(),
        };
        assert!(address_of(&record("http://127.0.0.1:12000")).is_some());
        assert!(address_of(&record("http://[::1]:12000")).is_some());
        assert!(address_of(&record("http://0.0.0.0:12000")).is_none());
        assert!(address_of(&record("http://192.168.1.5:12000")).is_none());
        assert!(address_of(&record("http://example.com:12000")).is_none());
    }

    #[test]
    fn a_signature_input_is_read_back() {
        let (label, params) = parse_input(
            "sig1=(\"@method\" \"@path\" \"content-digest\");created=1;expires=2;keyid=\"ab\";alg=\"ed25519\"",
        )
        .expect("parses");
        assert_eq!(
            (label.as_str(), params.created, params.expires),
            ("sig1", 1, 2)
        );
        assert_eq!(params.keyid, "ab");
        assert!(parse_input("sig1=(\"@method\");created=1;expires=2;keyid=\"ab\"").is_none());
    }
}
