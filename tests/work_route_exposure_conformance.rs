//! Conformance contract for every `RouteExposureV1::Public` executable binding.
//!
//! Each available executable binding advertises a public route path to clients.
//! This suite proves those paths are actually served by the merged production
//! HTTP application router — the router returned by
//! [`tracedecay::application_surface::http_application_router`], which is the
//! same constructor the daemon's `build_http_application_router` calls to mount
//! a project, and which the daemon serves without a path prefix. Nothing here
//! builds a substitute router, reads Rust source text, or accepts a descriptor,
//! a mount flag, or a caller boolean as evidence.
//!
//! The registry is composed with every route family declared. Production passes
//! `false` for the application-route family today, which turns those bindings
//! into `Unavailable` records — a truthful catalog, but a caller boolean cannot
//! settle whether a handler exists. Enumerating the fully declared set is what
//! makes the missing handlers visible instead of merely undeclared.
//!
//! Mounting is decided at axum's routing table, not by handler behavior. A
//! canonical binding is served as `POST`, so a `GET` to the same path can only
//! answer `405 Method Not Allowed` when the path is registered; an unregistered
//! path falls through to the router's empty-bodied `404`. That discriminator is
//! immune to application semantics, which matters because a mounted handler may
//! itself answer `404` for `NotFoundOrNotAuthorized` concealment. Every route is
//! probed both ways and the two signals must agree.
//!
//! Request bodies are derived from the request schema each binding publishes
//! (`ExecutableBindingV1::request_schema`), so no handwritten payload mirror can
//! drift from the wire contract.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Map, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use tracedecay::application::operation_stream::OperationEventAuthority;
use tracedecay::application_surface::http_application_router;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::DaemonInvocationClient;
use tracedecay_application::work_executable_binding_registry;
use tracedecay_domain::ProjectId;
use tracedecay_tool_catalog::RouteExposureV1;

/// Pins the global database away from the operator's profile. The production
/// constant is crate-private, so the name is repeated here for the same reason
/// the shared test harness repeats it.
const GLOBAL_DB_ENV: &str = "TRACEDECAY_GLOBAL_DB";

/// A path that no canonical binding declares, used to prove the router really
/// answers `404` for unmounted paths instead of swallowing everything.
const ABSENT_PROBE_PATH: &str = "/application/work/route-exposure-conformance-absent";

/// Route owned by `tracedecay_api::application_router`, the first of the three
/// routers merged into the production HTTP application router.
const API_ROUTER_WITNESS_PATH: &str = "/primitives/storage_status";

/// Route owned by the operation-event router, the third merged router.
const OPERATION_EVENT_WITNESS_PATH: &str = "/operations/operation.conformance/cancel";

/// Guards against a malformed schema cycle producing an unbounded instance.
const MAX_SCHEMA_DEPTH: usize = 32;

/// Restores a process environment variable when the guard drops.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // This binary hosts a single test, so no other thread reads the
        // environment while it is being pinned.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A live daemon under a throwaway profile, so the production invocation client
/// this router requires can be constructed without touching the operator's
/// TraceDecay data.
struct IsolatedDaemon {
    daemon: Child,
    project: PathBuf,
    _home: TempDir,
    _guards: Vec<EnvVarGuard>,
}

impl IsolatedDaemon {
    fn start() -> Self {
        let home = tempfile::tempdir().expect("isolated home");
        let root = home.path().to_path_buf();
        let profile = root.join(".tracedecay");
        let project = root.join("project");
        std::fs::create_dir_all(&profile).expect("isolated profile root");
        std::fs::create_dir_all(&project).expect("isolated project root");

        let guards = vec![
            EnvVarGuard::set("HOME", &root),
            EnvVarGuard::set("USERPROFILE", &root),
            EnvVarGuard::set("XDG_CONFIG_HOME", root.join(".config")),
            EnvVarGuard::set(USER_DATA_DIR_ENV, &profile),
            EnvVarGuard::set(GLOBAL_DB_ENV, profile.join("global.db")),
            EnvVarGuard::set("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1"),
        ];

        let mut daemon = Command::new(env!("CARGO_BIN_EXE_tracedecay"))
            .args(["daemon", "run"])
            .env("HOME", &root)
            .env("USERPROFILE", &root)
            .env("XDG_CONFIG_HOME", root.join(".config"))
            .env(USER_DATA_DIR_ENV, &profile)
            .env(GLOBAL_DB_ENV, profile.join("global.db"))
            .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1")
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon should start");

        Self {
            daemon,
            project,
            _home: home,
            _guards: guards,
        }
    }

    /// Waits until the daemon has published an authority record the production
    /// client can resolve, then returns that client.
    fn await_client(&mut self, project: &Path) -> DaemonInvocationClient {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = String::new();
        while Instant::now() < deadline {
            if let Some(status) = self.daemon.try_wait().expect("daemon status") {
                let mut stderr = String::new();
                if let Some(mut piped) = self.daemon.stderr.take() {
                    let _ = piped.read_to_string(&mut stderr);
                }
                panic!("daemon exited before publishing authority: {status}; stderr: {stderr}");
            }
            let handshake = DaemonHandshake::for_current_client(
                Some(project.to_path_buf()),
                None,
                false,
                false,
            )
            .expect("production daemon handshake");
            match DaemonInvocationClient::for_current(handshake) {
                Ok(client) => return client,
                Err(error) => last = error.to_string(),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon never published a resolvable authority record: {last}");
    }
}

impl Drop for IsolatedDaemon {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// One canonical public binding and the two independent routing observations
/// taken against the production router.
#[derive(Debug)]
struct RouteObservation {
    operation_id: String,
    route_path: String,
    /// Status for a method the route does not serve. `405` proves the path is
    /// in the routing table; `404` proves it is absent.
    method_mismatch_status: StatusCode,
    /// Status for the representative schema-derived `POST`.
    request_status: StatusCode,
    /// Body length of the representative `POST` response. The router's fallback
    /// `404` is empty; a handler's `404` carries a problem envelope.
    request_body_len: usize,
}

impl RouteObservation {
    /// Whether axum's routing table holds this path, judged only by the
    /// method-mismatch probe.
    fn path_is_registered(&self) -> bool {
        self.method_mismatch_status == StatusCode::METHOD_NOT_ALLOWED
    }

    /// Whether the representative request reached a handler rather than the
    /// router's empty fallback.
    fn request_reached_a_handler(&self) -> bool {
        self.request_status != StatusCode::NOT_FOUND || self.request_body_len > 0
    }

    fn describe(&self) -> String {
        format!(
            "{} -> {} (method-mismatch {}, representative POST {} with {} body bytes)",
            self.operation_id,
            self.route_path,
            self.method_mismatch_status.as_u16(),
            self.request_status.as_u16(),
            self.request_body_len
        )
    }
}

/// Every public route path the canonical executable catalog can advertise must
/// be served by the merged production HTTP application router.
#[tokio::test(flavor = "multi_thread")]
async fn public_executable_routes_are_served_by_the_production_http_router() {
    let mut fixture = IsolatedDaemon::start();
    let project = fixture.project.clone();
    let client = fixture.await_client(&project);

    let router = http_application_router(
        client,
        OperationEventAuthority::default(),
        ProjectId::new("project.route-exposure-conformance").expect("conformance project identity"),
    )
    .expect("production merged HTTP application router");

    assert_router_is_the_merged_production_router(&router).await;

    let registry =
        work_executable_binding_registry(true, true).expect("canonical Work binding registry");
    let mut declared_routes = BTreeMap::new();
    let mut withheld = Vec::new();
    for availability in registry.iter() {
        let Some(binding) = availability.binding() else {
            withheld.push(availability.operation_id().as_str().to_owned());
            continue;
        };
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            continue;
        };
        let body = representative_request_body(binding.request_schema().body());
        let previous = declared_routes.insert(
            binding.operation_id().as_str().to_owned(),
            (route_path.clone(), body),
        );
        assert!(
            previous.is_none(),
            "canonical registry declared {} twice",
            binding.operation_id().as_str()
        );
    }
    // Composed with every route family declared, so a withheld entry would
    // shrink the set under test instead of being reported.
    assert!(
        withheld.is_empty(),
        "the canonical registry withheld {} executable binding(s) even with \
         every route family declared, so the route probes below would grade a \
         silently truncated set: {}",
        withheld.len(),
        withheld.join(", ")
    );
    assert!(
        !declared_routes.is_empty(),
        "the canonical registry advertised no public executable routes, so this \
         contract would pass vacuously"
    );

    let mut observations = Vec::with_capacity(declared_routes.len());
    for (operation_id, (route_path, body)) in declared_routes {
        let (method_mismatch_status, _) = probe(&router, "GET", &route_path, None).await;
        let (request_status, request_body_len) =
            probe(&router, "POST", &route_path, Some(body)).await;
        observations.push(RouteObservation {
            operation_id,
            route_path,
            method_mismatch_status,
            request_status,
            request_body_len,
        });
    }

    for observation in &observations {
        assert!(
            matches!(
                observation.method_mismatch_status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "the method-mismatch probe answered neither 404 nor 405, so it no \
             longer discriminates a mounted path. A binding served on GET as \
             well as POST would do this; give such a binding a probe method it \
             does not serve instead of relaxing this check: {}",
            observation.describe()
        );
        assert_eq!(
            observation.path_is_registered(),
            observation.request_reached_a_handler(),
            "the routing-table and representative-request signals disagree, so \
             neither can be trusted: {}",
            observation.describe()
        );
    }

    let missing = observations
        .iter()
        .filter(|observation| !observation.path_is_registered())
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "{} of {} canonical public executable routes are advertised by the \
         catalog but have no handler mounted on the production HTTP \
         application router.\n\nmissing routes:\n{}\n\nmounted routes:\n{}",
        missing.len(),
        observations.len(),
        missing
            .iter()
            .map(|observation| format!("  {}", observation.describe()))
            .collect::<Vec<_>>()
            .join("\n"),
        observations
            .iter()
            .filter(|observation| observation.path_is_registered())
            .map(|observation| format!("  {}", observation.describe()))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Establishes that the router under test is the merged production router and
/// that `404` really distinguishes an unmounted path.
///
/// Without these preconditions a `404`-based verdict is meaningless: a router
/// with a catch-all would never answer `404`, and a router missing a merge arm
/// would report absent routes that production actually serves. Each merged arm
/// is witnessed by a route only that arm owns.
async fn assert_router_is_the_merged_production_router(router: &axum::Router) {
    let (absent_get, absent_get_len) = probe(router, "GET", ABSENT_PROBE_PATH, None).await;
    assert_eq!(
        absent_get,
        StatusCode::NOT_FOUND,
        "an undeclared path must answer 404, otherwise no route probe can \
         distinguish a mounted handler from a catch-all"
    );
    assert_eq!(
        absent_get_len, 0,
        "the router fallback must answer 404 with an empty body for the \
         body-length signal to separate a fallback from a handler problem"
    );
    let (absent_post, _) = probe(
        router,
        "POST",
        ABSENT_PROBE_PATH,
        Some(Value::Object(Map::new())),
    )
    .await;
    assert_eq!(
        absent_post,
        StatusCode::NOT_FOUND,
        "an undeclared path must answer 404 for the served method too"
    );

    for (witness, arm) in [
        (
            API_ROUTER_WITNESS_PATH,
            "tracedecay_api::application_router",
        ),
        (OPERATION_EVENT_WITNESS_PATH, "the operation-event router"),
    ] {
        let (status, _) = probe(router, "GET", witness, None).await;
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{witness} is served only by {arm}; a 405 here proves that arm was \
             merged into the router under test. Got {status} instead, so this \
             is not the merged production router."
        );
    }
}

/// Issues one request against the production router and returns the status and
/// response body length.
async fn probe(
    router: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, usize) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("route probe request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("route probe response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("route probe body");
    (status, bytes.len())
}

/// Builds a representative instance of the request schema the binding publishes.
///
/// Only required properties are populated: the Work request types deny unknown
/// fields and default their optional ones, so a minimal instance is the closest
/// thing to a canonical request the schema alone can express.
fn representative_request_body(schema: &Value) -> Value {
    schema_instance(schema, schema, 0)
}

fn schema_instance(schema: &Value, root: &Value, depth: usize) -> Value {
    let Some(object) = schema.as_object() else {
        return Value::Null;
    };
    if depth >= MAX_SCHEMA_DEPTH {
        return Value::Null;
    }
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        return match resolve_reference(reference, root) {
            Some(target) => schema_instance(target, root, depth + 1),
            None => Value::Null,
        };
    }
    if let Some(constant) = object.get("const") {
        return constant.clone();
    }
    if let Some(first) = object
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    for combinator in ["oneOf", "anyOf"] {
        if let Some(first) = object
            .get(combinator)
            .and_then(Value::as_array)
            .and_then(|branches| branches.first())
        {
            return schema_instance(first, root, depth + 1);
        }
    }
    if let Some(branches) = object.get("allOf").and_then(Value::as_array) {
        let mut merged = Map::new();
        for branch in branches {
            if let Value::Object(fields) = schema_instance(branch, root, depth + 1) {
                merged.extend(fields);
            }
        }
        return Value::Object(merged);
    }
    match declared_type(object) {
        Some("object") => object_instance(object, root, depth),
        Some("array") => array_instance(object, root, depth),
        Some("string") => Value::String(string_instance(object)),
        Some("integer" | "number") => number_instance(object),
        Some("boolean") => Value::Bool(false),
        Some("null") => Value::Null,
        _ if object.contains_key("properties") || object.contains_key("required") => {
            object_instance(object, root, depth)
        }
        _ => Value::Null,
    }
}

/// The first non-null entry of `type`, which schemars emits either as a bare
/// string or as a union with `"null"` for optional fields.
fn declared_type(object: &Map<String, Value>) -> Option<&str> {
    match object.get("type") {
        Some(Value::String(name)) => Some(name.as_str()),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    }
}

fn resolve_reference<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    let mut current = root;
    for segment in reference.strip_prefix("#/")?.split('/') {
        current = current.get(segment.replace("~1", "/").replace("~0", "~"))?;
    }
    Some(current)
}

fn object_instance(object: &Map<String, Value>, root: &Value, depth: usize) -> Value {
    let properties = object.get("properties").and_then(Value::as_object);
    let mut instance = Map::new();
    for name in object
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let property = properties
            .and_then(|properties| properties.get(name))
            .map_or(Value::Null, |schema| {
                schema_instance(schema, root, depth + 1)
            });
        instance.insert(name.to_owned(), property);
    }
    Value::Object(instance)
}

fn array_instance(object: &Map<String, Value>, root: &Value, depth: usize) -> Value {
    let minimum = object
        .get("minItems")
        .and_then(Value::as_u64)
        .and_then(|minimum| usize::try_from(minimum).ok())
        .unwrap_or_default();
    let items = match object.get("items") {
        Some(items) => schema_instance(items, root, depth + 1),
        None => Value::Null,
    };
    Value::Array(vec![items; minimum])
}

fn string_instance(object: &Map<String, Value>) -> String {
    let mut value = match object.get("format").and_then(Value::as_str) {
        Some("date-time") => "1970-01-01T00:00:00Z".to_owned(),
        Some("uuid") => "00000000-0000-0000-0000-000000000000".to_owned(),
        _ => "route-exposure-conformance".to_owned(),
    };
    let minimum = object
        .get("minLength")
        .and_then(Value::as_u64)
        .and_then(|minimum| usize::try_from(minimum).ok())
        .unwrap_or_default();
    while value.len() < minimum {
        value.push('x');
    }
    if let Some(maximum) = object
        .get("maxLength")
        .and_then(Value::as_u64)
        .and_then(|maximum| usize::try_from(maximum).ok())
        && value.len() > maximum
    {
        value.truncate(maximum);
    }
    value
}

fn number_instance(object: &Map<String, Value>) -> Value {
    let mut chosen = object
        .get("minimum")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .max(1);
    if let Some(maximum) = object.get("maximum").and_then(Value::as_i64)
        && chosen > maximum
    {
        chosen = maximum;
    }
    Value::from(chosen)
}
