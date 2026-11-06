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
    /// A Warm Standby's reservation (spec §7): a spawn of the member that waits.
    Standby,
    Availability,
    Extend,
    /// The extension of a reserved standby, bare like `Extend`.
    StandbyExtend,
    Status,
    Terminate,
    Rotate,
}

impl Route {
    /// The `op` a Lease Request on this route carries; a route with no Lease Request has none.
    fn op(self) -> Option<&'static str> {
        match self {
            Route::Spawn => Some("spawn"),
            Route::Standby => Some("standby"),
            Route::Status => Some("status"),
            Route::Terminate => Some("terminate"),
            Route::Rotate => Some("rotate"),
            Route::Availability | Route::Extend | Route::StandbyExtend => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Route::Spawn => "spawn",
            Route::Standby => "standby",
            Route::Availability => "availability",
            Route::Extend => "extend",
            Route::StandbyExtend => "standby.extend",
            Route::Status => "status",
            Route::Terminate => "terminate",
            Route::Rotate => "rotate",
        }
    }
}

/// The prefix of an ILP address a tenant pays for `route` of `listing`'s `version` at the
/// provider `ilp_address`.
pub fn destination(ilp_address: &str, route: Route, listing: &str, version: u64) -> String {
    match route {
        Route::Spawn | Route::Extend | Route::Standby | Route::StandbyExtend => {
            format!("{ilp_address}.{listing}.v{version}.{}", route.name())
        }
        Route::Availability | Route::Status | Route::Terminate | Route::Rotate => {
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

/// The content of a spawn: what the tenant asked to run, under the lease's `workload_id` and,
/// for a member of a Standby Set, the `standby_set` (spec §7): every member's public key,
/// the primary first, the same for each member.
pub fn member_content(base: &Value, workload_id: &str, standby_set: &[String]) -> Value {
    let mut content = base.clone();
    content["workload_id"] = json!(workload_id);
    if !standby_set.is_empty() {
        content["standby_set"] = json!(standby_set);
    }
    content
}

/// The `content` of a rotate (§6.8): `next` is the token `member` holds from the new root
/// secret once it has confirmed.
pub fn rotate_content(workload_id: &str, next: &[u8; 32], member: &str) -> Value {
    json!({ "workload_id": workload_id, "next": continuation(next, member) })
}

/// Whether the body a rotate was answered with confirms it (`rotate.ok`).
pub fn rotated(body: &Value) -> bool {
    body["rotated"] == json!(true)
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
/// carries `{ "request": <Lease Request> }` and nothing else, never a bare content object; an
/// extend is the opposite: the bare `{ "workload_id": … }`, never wrapped in `request`.
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
        Route::Extend | Route::StandbyExtend => {
            only_keys(object, &["workload_id"], &["workload_id"]).and_then(|()| {
                if is_hex(&object["workload_id"], 64) {
                    Ok(())
                } else {
                    Err("`workload_id` is not 64 lowercase hex characters".into())
                }
            })
        }
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
    if route == Route::Rotate {
        only_keys(
            request["content"].as_object().expect("an object"),
            &["workload_id", "next"],
            &["workload_id", "next"],
        )?;
        for field in ["workload_id", "next"] {
            if !is_hex(&request["content"][field], 64) {
                return Err(format!(
                    "`content.{field}` is not 64 lowercase hex characters"
                ));
            }
        }
        if request["content"]["next"] == request["continuation"] {
            return Err("`next` is the token the request presents".into());
        }
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
            "lease_request.rotate",
        ] {
            let (built, recorded) = rebuilt(name);
            assert_eq!(built, recorded, "{name}");
            assert_eq!(built.to_string(), recorded.to_string(), "{name}");
        }
    }

    /// A member's request as `fixture` records it, built again over the content of
    /// `lease_request.standby` with that member's set and workload id.
    fn member_body(fixture_name: &str) -> (Value, Value) {
        let fixture = fixture(fixture_name);
        let request = &fixture["request_body"]["request"];
        let base = &self::fixture("lease_request.standby")["request"]["content"];
        let mut base = base.clone();
        base.as_object_mut().unwrap().remove("standby_set");
        let set: Vec<String> = request["content"]["standby_set"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| text(key).to_owned())
            .collect();
        let built = Request {
            op: text(&request["op"]),
            request_id: text(&request["request_id"]),
            provider: text(&request["provider"]),
            expiration: request["expiration"].as_u64().unwrap(),
            continuation: text(&request["continuation"]),
            content: member_content(&base, text(&request["content"]["workload_id"]), &set),
        };
        (built.body(), fixture["request_body"].clone())
    }

    #[test]
    fn a_standby_is_a_request_of_op_standby_at_its_own_route() {
        let (built, recorded) = rebuilt("lease_request.standby");
        assert_eq!(built, recorded);
        check_shape(Route::Standby, &built).expect("standby");
        assert!(check_shape(Route::Spawn, &built).is_err());
        for name in ["spawn.standby", "spawn.primary"] {
            let (built, recorded) = member_body(name);
            assert_eq!(built.to_string(), recorded.to_string(), "{name}");
        }
        check_shape(Route::Standby, &member_body("spawn.standby").0).expect("standby");
        check_shape(Route::Spawn, &member_body("spawn.primary").0).expect("primary");
        let spawn = fixture("spawn.standby");
        assert_eq!(
            destination("g.fixture", Route::Standby, "warm", 1),
            spawn["route"].as_str().unwrap()
        );
        let primary = fixture("spawn.primary");
        assert_eq!(
            destination("g.fixture", Route::Spawn, "warm", 1),
            primary["route"].as_str().unwrap()
        );
        let extend = fixture("standby_extend.ok");
        check_shape(Route::StandbyExtend, &extend["request_body"]).expect("standby extend");
        assert_eq!(
            destination("g.fixture", Route::StandbyExtend, "warm", 1),
            extend["route"].as_str().unwrap()
        );
        let wrapped =
            json!({ "request": { "workload_id": extend["request_body"]["workload_id"] } });
        assert!(check_shape(Route::StandbyExtend, &wrapped).is_err());
        check_shape(
            Route::Availability,
            &fixture("availability.standby_role")["request_body"],
        )
        .expect("availability with a role");
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
            ("lease_request.rotate", Route::Rotate),
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
            ("lease_request.rotate", Route::Rotate),
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
    fn an_extend_is_bare_and_a_wrapped_one_is_refused() {
        let fixture = fixture("extend.ok");
        let body = &fixture["request_body"];
        check_shape(Route::Extend, body).expect("the fixture's body");
        let wrapped = json!({ "request": { "workload_id": body["workload_id"] } });
        let error = check_shape(Route::Extend, &wrapped).expect_err("wrapped");
        assert_eq!(error.code, ErrorCode::LeaseBodyShape);
        assert!(error.nothing_sent);
        assert!(check_shape(Route::Extend, &json!({ "workload_id": "aa" })).is_err());
        assert!(check_shape(Route::Spawn, body).is_err());
        assert_eq!(
            destination("g.fixture", Route::Extend, "basic", 1),
            fixture["route"].as_str().unwrap()
        );
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

    #[test]
    fn a_rotate_derives_its_next_from_the_new_secret_and_its_answer_is_read_as_success() {
        let constants = fixture("constants");
        let next: [u8; 32] = hex::decode(text(&constants["rotated_tenant"]["root_secret"]))
            .unwrap()
            .try_into()
            .unwrap();
        let recorded = fixture("lease_request.rotate");
        assert_eq!(
            rotate_content(
                text(&recorded["request"]["content"]["workload_id"]),
                &next,
                text(&constants["provider"]["public_key"]),
            ),
            recorded["request"]["content"]
        );
        assert!(rotated(&fixture("rotate.ok")["response_body"]));
        assert!(!rotated(&json!({ "workload_id": "aa".repeat(32) })));
    }

    #[test]
    fn a_rotate_is_read_as_the_fixture_and_checked_for_its_next() {
        let body = fixture("lease_request.rotate")["packet_body"].clone();
        let mut same = body.clone();
        same["request"]["content"]["next"] = same["request"]["continuation"].clone();
        assert!(check_shape(Route::Rotate, &same).is_err());
        let mut short = body.clone();
        short["request"]["content"]["next"] = json!("abc");
        assert!(check_shape(Route::Rotate, &short).is_err());
        let mut missing = body;
        missing["request"]["content"] = json!({ "workload_id": "aa".repeat(32) });
        assert!(check_shape(Route::Rotate, &missing).is_err());
        assert_eq!(
            destination("g.fixture", Route::Rotate, "basic", 1),
            "g.fixture.rotate"
        );
    }
}
