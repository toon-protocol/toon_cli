//! The Lease Request of the TOON Compute protocol (spec §5, §6.1): what a paid or an
//! authenticated route carries, and the check every packet's body passes before it leaves.
//!
//! A Lease Request is a plain JSON object that nobody signs. It carries the lease's
//! Continuation Token, a secret the tenant minted and derived from the lease's root secret,
//! one per provider (§6.1.1). The fixtures in `vendor/toon-network-fixtures/` are the test
//! vectors for every function here.

use hkdf::Hkdf;
use serde_json::{json, Map, Value};
use sha2::Sha256;

use crate::outcome::{Error, ErrorCode};

/// The routes of a provider (spec §5) and the shape of the body each takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Spawn,
    Availability,
    Status,
    Terminate,
}

impl Route {
    /// The `op` a Lease Request on this route carries; a route with no Lease Request has none.
    fn op(self) -> Option<&'static str> {
        match self {
            Route::Spawn => Some("spawn"),
            Route::Status => Some("status"),
            Route::Terminate => Some("terminate"),
            Route::Availability => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Route::Spawn => "spawn",
            Route::Availability => "availability",
            Route::Status => "status",
            Route::Terminate => "terminate",
        }
    }
}

/// The prefix of an ILP address a tenant pays for `route` of `listing`'s `version` at the
/// provider `ilp_address`.
pub fn destination(ilp_address: &str, route: Route, listing: &str, version: u64) -> String {
    match route {
        Route::Spawn => format!("{ilp_address}.{listing}.v{version}.{}", route.name()),
        Route::Availability | Route::Status | Route::Terminate => {
            format!("{ilp_address}.{}", route.name())
        }
    }
}

/// `continuation(provider)` (spec §6.1.1): HKDF-SHA256 over the lease's root secret, an empty
/// salt and `toon-network-continuation:` followed by the provider's 64 lowercase hex
/// characters, for 32 bytes, as 64 lowercase hex characters.
pub fn continuation(root: &[u8; 32], provider: &str) -> String {
    let mut token = [0u8; 32];
    Hkdf::<Sha256>::new(None, root)
        .expand(
            format!("toon-network-continuation:{provider}").as_bytes(),
            &mut token,
        )
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    hex::encode(token)
}

/// A Lease Request, the object that is the value of `request` in a body.
pub struct Request<'a> {
    pub op: &'a str,
    /// 32 random bytes, as 64 lowercase hex characters.
    pub request_id: &'a str,
    /// The one provider this request is for, 64 lowercase hex characters.
    pub provider: &'a str,
    /// Unix seconds.
    pub expiration: u64,
    pub continuation: &'a str,
    pub content: Value,
}

impl Request<'_> {
    pub fn build(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "op": self.op,
            "provider": self.provider,
            "expiration": self.expiration,
            "continuation": self.continuation,
            "content": self.content,
        })
    }

    /// The packet body: the request, unmodified, as the value of the single key `request`.
    pub fn body(&self) -> Value {
        json!({ "request": self.build() })
    }
}

fn is_hex(value: &Value, length: usize) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == length
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn refused(route: Route, why: String) -> Error {
    Error {
        nothing_sent: true,
        unanswered: None,
        code: ErrorCode::LeaseBodyShape,
        message: format!(
            "The body for the {} route is not the shape the provider reads: {why}. Nothing was \
             sent, because a provider bills a body of the wrong shape a full Lease Interval and \
             refunds nothing.",
            route.name()
        ),
    }
}

fn only_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), String> {
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("`{unknown}` is a field this route does not name"));
    }
    match required.iter().find(|key| !object.contains_key(**key)) {
        Some(missing) => Err(format!("`{missing}` is missing")),
        None => Ok(()),
    }
}

/// Check `body` against spec §5's table for `route`, before the packet leaves: the shape is
/// the one mistake that is free to catch and expensive to make. A spawn, status or terminate
/// carries `{ "request": <Lease Request> }` and nothing else, never a bare content object.
pub fn check_shape(route: Route, body: &Value) -> Result<(), Error> {
    let object = body
        .as_object()
        .ok_or_else(|| refused(route, "it is not a JSON object".into()))?;
    let checked = match route {
        Route::Availability => only_keys(
            object,
            &["listing", "version", "image", "role"],
            &["listing", "version", "image"],
        ),
        authenticated => check_request(authenticated, object),
    };
    checked.map_err(|why| refused(route, why))
}

fn check_request(route: Route, body: &Map<String, Value>) -> Result<(), String> {
    only_keys(body, &["request"], &["request"]).map_err(|why| {
        format!("{why}, and a Lease Request is the value of the single key `request`")
    })?;
    let request = body["request"]
        .as_object()
        .ok_or("`request` is not an object")?;
    only_keys(
        request,
        &[
            "request_id",
            "op",
            "provider",
            "expiration",
            "continuation",
            "content",
        ],
        &[
            "request_id",
            "op",
            "provider",
            "expiration",
            "continuation",
            "content",
        ],
    )?;
    if request["op"].as_str() != route.op() {
        return Err(format!(
            "`op` is {}, and the {} route serves {}",
            request["op"],
            route.name(),
            route.op().unwrap_or_default()
        ));
    }
    for (field, length) in [("request_id", 64), ("provider", 64), ("continuation", 64)] {
        if !is_hex(&request[field], length) {
            return Err(format!(
                "`{field}` is not {length} lowercase hex characters"
            ));
        }
    }
    if !request["expiration"].is_u64() {
        return Err("`expiration` is not unix seconds".into());
    }
    if !request["content"].is_object() {
        return Err("`content` is not an object".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture(name: &str) -> Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/toon-network-fixtures")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(&path).expect("a vendored fixture"))
            .expect("a JSON fixture")
    }

    fn text(value: &Value) -> &str {
        value.as_str().expect("text")
    }

    /// The request a fixture records, built again from its parts.
    fn rebuilt(name: &str) -> (Value, Value) {
        let fixture = fixture(name);
        let request = &fixture["request"];
        let built = Request {
            op: text(&request["op"]),
            request_id: text(&request["request_id"]),
            provider: text(&request["provider"]),
            expiration: request["expiration"].as_u64().unwrap(),
            continuation: text(&request["continuation"]),
            content: request["content"].clone(),
        };
        (built.body(), fixture["packet_body"].clone())
    }

    #[test]
    fn a_spawn_status_or_terminate_is_byte_identical_to_the_fixture() {
        for name in [
            "lease_request.spawn",
            "lease_request.status",
            "lease_request.terminate",
        ] {
            let (built, recorded) = rebuilt(name);
            assert_eq!(built, recorded, "{name}");
            assert_eq!(built.to_string(), recorded.to_string(), "{name}");
        }
    }

    #[test]
    fn the_fixtures_are_vendored_at_one_full_commit() {
        let rev = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/toon-network-fixtures/REV"),
        )
        .expect("a REV file");
        let rev = rev.trim();
        assert!(
            rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{rev}"
        );
    }

    #[test]
    fn the_continuation_derivation_matches_the_vector() {
        let vector = fixture("continuation.vector");
        let root: [u8; 32] = hex::decode(text(&vector["root_secret"]))
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            continuation(&root, text(&vector["provider_public_key"])),
            text(&vector["continuation"])
        );
        assert_eq!(
            continuation(
                &root,
                text(&vector["at_other_provider"]["provider_public_key"])
            ),
            text(&vector["at_other_provider"]["continuation"])
        );
    }

    #[test]
    fn every_fixture_body_passes_the_shape_check_of_its_route() {
        for (name, route) in [
            ("lease_request.spawn", Route::Spawn),
            ("lease_request.status", Route::Status),
            ("lease_request.terminate", Route::Terminate),
        ] {
            let body = &fixture(name)["packet_body"];
            check_shape(route, body).unwrap_or_else(|error| panic!("{name}: {}", error.message));
        }
        let availability = fixture("availability.would_run");
        check_shape(Route::Availability, &availability["request_body"]).expect("availability");
    }

    #[test]
    fn a_spawn_status_or_terminate_built_bare_is_refused() {
        for (name, route) in [
            ("lease_request.spawn", Route::Spawn),
            ("lease_request.status", Route::Status),
            ("lease_request.terminate", Route::Terminate),
        ] {
            let bare = fixture(name)["request"]["content"].clone();
            let error = check_shape(route, &bare).expect_err(name);
            assert_eq!(error.code, ErrorCode::LeaseBodyShape);
            assert!(error.nothing_sent);
            // The request itself, not inside `request`, is bare too.
            let unwrapped = fixture(name)["request"].clone();
            assert!(check_shape(route, &unwrapped).is_err(), "{name}");
        }
    }

    #[test]
    fn a_request_for_another_op_is_refused() {
        let body = fixture("lease_request.status")["packet_body"].clone();
        assert!(check_shape(Route::Terminate, &body).is_err());
        assert!(check_shape(Route::Status, &body).is_ok());
    }

    #[test]
    fn a_route_is_paid_at_its_address() {
        assert_eq!(
            destination("g.fixture", Route::Spawn, "basic", 1),
            "g.fixture.basic.v1.spawn"
        );
        assert_eq!(
            destination("g.fixture", Route::Status, "basic", 1),
            "g.fixture.status"
        );
    }
}
