//! Conformance contract for every `RouteExposureV1::Public` executable binding.
//!
//! Each available executable binding advertises a public route path to clients.
//! Reachability is graded at the surface a client actually calls: the live
//! daemon's published HTTP application endpoint, authenticated with the token
//! and origin from its own authority record. The daemon exposes one outer route,
//! `/projects/{project_id}/application/{*tail}`, and
//! `dispatch_project_application` rewrites the inner router URI to `/{tail}`
//! (`src/daemon/http_application.rs`). A canonical `route_path` already starts
//! with `/application`, so the external URL is `/projects/{project_id}` followed
//! by the canonical path, and the inner router must therefore register that path
//! *relative* to the stripped prefix.
//!
//! Probing the inner router directly would bypass that rewrite and score a
//! double-prefixed route as mounted, so the in-process router is kept only as
//! secondary diagnostics, and any disagreement between inner and outer
//! reachability fails this gate. That is what stops a prefix regression from
//! hiding behind inner-router evidence.
//!
//! Mounting is decided at axum's routing table, not by handler behavior. A
//! canonical binding is served as `POST`, so a `GET` to the same path can only
//! answer `405 Method Not Allowed` when the path is registered; an unregistered
//! path falls through to the router's empty-bodied `404`. That discriminator is
//! immune to application semantics, which matters because a mounted handler may
//! itself answer `404` for `NotFoundOrNotAuthorized` concealment. Every route is
//! probed both ways and the two signals must agree.
//!
//! The registry has no caller-supplied mount flags. Every public binding must be
//! reachable on the production endpoint or this test fails.
//!
//! Request bodies are derived from the request schema each binding publishes
//! (`ExecutableBindingV1::request_schema`), so no handwritten payload mirror can
//! drift from the wire contract.

mod common;

#[path = "work_route_exposure_conformance/work_evidence.rs"]
mod work_evidence;
#[path = "work_route_exposure_conformance/work_task_session.rs"]
mod work_task_session;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Map, Value};
use tempfile::TempDir;
use tower::ServiceExt;
use tracedecay::application_surface::http_application_router;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::DaemonInvocationClient;
use tracedecay::storage::PrivateStoreIo;
use tracedecay_application::work_executable_binding_registry;
use tracedecay_domain::ProjectId;
use tracedecay_tool_catalog::RouteExposureV1;
use tracedecay_usecases::operation_stream::OperationEventAuthority;

/// Pins the global database away from the operator's profile. The production
/// constant is crate-private, so the name is repeated here for the same reason
/// the shared test harness repeats it.
const GLOBAL_DB_ENV: &str = "TRACEDECAY_GLOBAL_DB";

/// Tail that no canonical binding declares, used to prove the routers really
/// answer `404` for an unmounted path instead of swallowing everything.
const ABSENT_TAIL: &str = "/application/route-exposure-conformance-absent";

/// Canonical path owned by `tracedecay_api::application_router`, which registers
/// its routes relative to the outer prefix. Reaching it proves the outer
/// dispatch resolved the project, applied the URI rewrite, and handed off to the
/// inner routing table.
const RELATIVE_WITNESS_TAIL: &str = "/application/primitives/storage_status";

/// A project id the registry cannot resolve, used to show that an unresolved
/// project also answers `404` — which is why the witness probe above has to pass
/// before any per-route verdict is trusted.
const UNKNOWN_PROJECT_ID: &str = "project.route-exposure-conformance-unknown";

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
        // Every test in this binary pins the environment through
        // `ProductionDaemon::start`, which holds the shared env lock for the
        // fixture's whole life, so no other thread reads the environment while
        // it is being pinned.
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

/// A live daemon over a registered project under a throwaway profile, plus the
/// credentials it published for its own HTTP application endpoint.
struct ProductionDaemon {
    daemon: Child,
    project: PathBuf,
    project_id: String,
    base_url: String,
    origin: String,
    authorization: String,
    home: TempDir,
    profile: PathBuf,
    _guards: Vec<EnvVarGuard>,
    // Held for the fixture's whole life: starting a daemon pins process-wide
    // environment variables, and this binary now hosts more than one test.
    _env_lock: std::sync::MutexGuard<'static, ()>,
}

impl ProductionDaemon {
    fn start() -> Self {
        let env_lock = common::lock_global_db_env();
        let home = tempfile::tempdir().expect("isolated home");
        let root = home.path().to_path_buf();
        let profile = root.join(".tracedecay");
        let project = root.join("project");
        PrivateStoreIo::create_dir_all(&profile).expect("isolated profile root");
        fs::create_dir_all(project.join("src")).expect("isolated project root");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname=\"route-exposure-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
        )
        .expect("fixture manifest");
        fs::write(
            project.join("src/lib.rs"),
            "pub const ROUTE_EXPOSURE_FIXTURE: bool = true;\n",
        )
        .expect("fixture source");

        let guards = vec![
            EnvVarGuard::set("HOME", &root),
            EnvVarGuard::set("USERPROFILE", &root),
            EnvVarGuard::set("XDG_CONFIG_HOME", root.join(".config")),
            EnvVarGuard::set(USER_DATA_DIR_ENV, &profile),
            EnvVarGuard::set(GLOBAL_DB_ENV, profile.join("global.db")),
            EnvVarGuard::set("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1"),
        ];

        run_ok(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&project),
            "git init",
        );
        let mut daemon = isolated(&root, &profile)
            .args(["daemon", "run"])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon should start");
        let authority = wait_for_authority(&mut daemon, &common::daemon_authority_path(&profile));
        run_ok(
            isolated(&root, &profile).arg("init").current_dir(&project),
            "tracedecay init",
        );

        let context = run_ok(
            isolated(&root, &profile)
                .args(["projects", "context"])
                .arg(&project)
                .arg("--json")
                .current_dir(&project),
            "tracedecay projects context",
        );
        let context: Value = serde_json::from_slice(&context).expect("project context JSON");
        let project_id = context["project"]["project_id"]
            .as_str()
            .expect("registered project id")
            .to_owned();

        let endpoint = authority["http_application_endpoint"]
            .as_str()
            .expect("published HTTP application endpoint")
            .to_owned();
        let token = authority["auth_token"]
            .as_str()
            .expect("published auth token")
            .to_owned();

        Self {
            daemon,
            project,
            project_id,
            base_url: format!("http://{endpoint}"),
            origin: format!("http://{endpoint}"),
            authorization: format!("Bearer {token}"),
            home,
            profile,
            _guards: guards,
            _env_lock: env_lock,
        }
    }

    /// External URL for a canonical route path, which already starts with
    /// `/application` and therefore composes directly onto the project prefix.
    fn external_url(&self, route_path: &str) -> String {
        format!(
            "{}/projects/{}{}",
            self.base_url, self.project_id, route_path
        )
    }

    /// Kills the daemon process and starts a new one over the same profile.
    ///
    /// This is a physical restart, not a handle reset: the old process is
    /// reaped and the published authority record is removed first, so the new
    /// endpoint and token are the ones this daemon minted rather than a stale
    /// read of its predecessor's. Everything a journey asserts afterwards was
    /// therefore reconstructed from durable state.
    fn restart(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let authority_path = common::daemon_authority_path(&self.profile);
        let _ = fs::remove_file(&authority_path);
        let mut daemon = isolated(self.home.path(), &self.profile)
            .args(["daemon", "run"])
            .current_dir(&self.project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon should restart");
        let authority = wait_for_authority(&mut daemon, &authority_path);
        let endpoint = authority["http_application_endpoint"]
            .as_str()
            .expect("republished HTTP application endpoint")
            .to_owned();
        let token = authority["auth_token"]
            .as_str()
            .expect("republished auth token")
            .to_owned();
        self.daemon = daemon;
        self.base_url = format!("http://{endpoint}");
        self.origin = self.base_url.clone();
        self.authorization = format!("Bearer {token}");
    }
}

impl Drop for ProductionDaemon {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// A `tracedecay dashboard` process bound to the live daemon above.
///
/// The dashboard mounts `/api/work` only when it can resolve that daemon's
/// authority record; an in-process test server has none and degrades to serving
/// the core dashboard without the application surface. Running the real binary
/// against the real daemon is therefore the only place the browser-facing mount
/// exists to be tested.
struct DashboardProcess {
    process: Child,
    base_url: String,
}

impl DashboardProcess {
    fn start(fixture: &ProductionDaemon) -> Self {
        // Port 0 makes the server pick a free port and print it, which avoids
        // the bind race a pre-picked port would leave open.
        let mut process = isolated(fixture.home.path(), &fixture.profile)
            .args(["dashboard", "--host", "127.0.0.1", "--port", "0"])
            .current_dir(&fixture.project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("dashboard should start");
        let stdout = process.stdout.take().expect("dashboard stdout");
        let base_url = read_listening_url(stdout, &mut process);
        Self { process, base_url }
    }

    /// Rebinds the dashboard to a daemon that has just been restarted.
    ///
    /// The dashboard resolves the daemon's authority record when it starts, so
    /// a restarted daemon publishes credentials the running dashboard has
    /// never seen. Restarting it here keeps the mount honest instead of
    /// letting a stale forward masquerade as a passing assertion.
    fn restart(&mut self, fixture: &ProductionDaemon) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        *self = Self::start(fixture);
    }
}

impl Drop for DashboardProcess {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Reads the dashboard's announced listen URL off its stdout.
fn read_listening_url(stdout: std::process::ChildStdout, process: &mut Child) -> String {
    use std::io::{BufRead, BufReader};

    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut seen = String::new();
    let mut listening = None;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                seen.push_str(&line);
                if let Some(rest) = line.split_once("listening on ")
                    && let Some(url) = rest.1.split_whitespace().next()
                {
                    listening = Some(url.trim_end_matches('/').to_owned());
                    break;
                }
            }
            Err(error) => panic!("dashboard stdout failed: {error}\nseen:\n{seen}"),
        }
    }
    if let Some(url) = listening {
        // Keep draining the pipe for the server's whole life. Dropping the read
        // end here would turn the dashboard's next stdout write into SIGPIPE,
        // killing the very mount the journey is about to exercise — a failure
        // that surfaces later as a connection refusal with no cause attached.
        std::thread::spawn(move || {
            let mut line = String::new();
            while matches!(reader.read_line(&mut line), Ok(read) if read > 0) {
                line.clear();
            }
        });
        return url;
    }
    let mut stderr = String::new();
    if let Some(mut piped) = process.stderr.take() {
        let _ = piped.read_to_string(&mut stderr);
    }
    panic!("dashboard never announced a listen URL\nstdout:\n{seen}\nstderr:\n{stderr}");
}

fn isolated(home: &Path, profile: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(USER_DATA_DIR_ENV, profile)
        .env(GLOBAL_DB_ENV, profile.join("global.db"))
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1");
    command
}

fn run_ok(command: &mut Command, label: &str) -> Vec<u8> {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label} could not run: {error}"));
    assert!(
        output.status.success(),
        "{label} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn wait_for_authority(daemon: &mut Child, path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if let Some(status) = daemon.try_wait().expect("daemon status") {
            let mut stderr = String::new();
            if let Some(mut piped) = daemon.stderr.take() {
                let _ = piped.read_to_string(&mut stderr);
            }
            panic!("daemon exited before publishing authority: {status}; stderr: {stderr}");
        }
        if let Ok(bytes) = fs::read(path)
            && let Ok(record) = serde_json::from_slice::<Value>(&bytes)
            && record["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
            && record["http_application_endpoint"].as_str().is_some()
        {
            return record;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for a published HTTP application endpoint at {}",
        path.display()
    );
}

/// Status and body length of one probe, the two facts every verdict rests on.
#[derive(Clone, Copy, Debug)]
struct ProbeResult {
    status: u16,
    body_len: usize,
}

impl ProbeResult {
    /// Whether the routing table holds the path. Only a method-mismatch probe
    /// may be judged this way.
    fn path_is_registered(self) -> bool {
        self.status == StatusCode::METHOD_NOT_ALLOWED.as_u16()
    }

    /// Whether a request reached a handler rather than a router fallback, which
    /// always answers `404` with an empty body.
    fn reached_a_handler(self) -> bool {
        self.status != StatusCode::NOT_FOUND.as_u16() || self.body_len > 0
    }
}

/// One canonical public binding graded at the external surface, with the
/// in-process router recorded alongside as secondary evidence.
#[derive(Debug)]
struct RouteObservation {
    operation_id: String,
    route_path: String,
    external_url: String,
    outer_method_mismatch: ProbeResult,
    outer_request: ProbeResult,
    inner_method_mismatch: ProbeResult,
    inner_request: ProbeResult,
}

impl RouteObservation {
    fn describe(&self) -> String {
        format!(
            "{} -> {}\n      external {}: GET {} / POST {} ({} body bytes)\n      inner    {}: GET {} / POST {} ({} body bytes)",
            self.operation_id,
            self.route_path,
            self.external_url,
            self.outer_method_mismatch.status,
            self.outer_request.status,
            self.outer_request.body_len,
            self.route_path,
            self.inner_method_mismatch.status,
            self.inner_request.status,
            self.inner_request.body_len,
        )
    }
}

/// A cursor that no live topology generation ever matches, used to prove the
/// stale-cursor refusal on both mounts. The identity fields are well-formed so
/// the request decodes and the staleness verdict is the handler's, not serde's.
fn superseded_cursor_request() -> Value {
    serde_json::json!({
        "page_size": 25,
        "cursor": {
            "generation": "work-topology/route-exposure-superseded",
            "start_after": {
                "task_id": "task.work-surface-conformance",
                "run_id": "run.work-surface-conformance",
                "attempt_id": "attempt.work-surface-conformance.1",
            },
        },
    })
}

fn product_selection() -> Value {
    serde_json::json!({ "selection": "profile_owned_no_git" })
}

fn current_product_graph_request(observed_at: i64) -> Value {
    serde_json::json!({
        "selection": product_selection(),
        "mode": { "mode": "current" },
        "continuation": null,
        "observed_at": observed_at,
    })
}

fn product_task_create_draft() -> Value {
    let occurred_at = 1_700_000_000_000_000i64;
    serde_json::json!({
        "selection": product_selection(),
        "causation_event_id": null,
        "evidence": [],
        "change": {
            "change": "create_task",
            "initiative": {
                "id": "initiative.work-surface-conformance",
                "title": "Work surface conformance",
                "created_at": occurred_at,
            },
            "plan": {
                "id": "plan.work-surface-conformance",
                "initiative_id": "initiative.work-surface-conformance",
                "title": "Work surface conformance",
                "created_at": occurred_at,
            },
            "milestone": {
                "id": "milestone.work-surface-conformance",
                "plan_id": "plan.work-surface-conformance",
                "title": "Work surface conformance",
                "created_at": occurred_at,
            },
            "item": {
                "input": {
                    "task_id": "task.work-surface-conformance",
                    "hierarchy": {
                        "initiative_id": "initiative.work-surface-conformance",
                        "plan_id": "plan.work-surface-conformance",
                        "milestone_id": "milestone.work-surface-conformance",
                    },
                    "title": "Work surface conformance",
                    "dependencies": [],
                    "informational_relations": [],
                    "causal_candidates": [],
                    "acceptance_criteria": [],
                    "effort": 1,
                    "scheduled_at": null,
                    "deadline": null,
                    "created_at": occurred_at,
                    "updated_at": occurred_at,
                },
                "accepted_proposal": null,
                "accepted_route": null,
                "execution_admitted_at": null,
                "evidence_links": [],
                "accepted_criteria": {},
                "accepted_attempts": [],
                "handoffs": [],
                "accepted_at": null,
                "archived_at": null,
            },
        },
    })
}

/// Grades one answer against the canonical envelope the clients decode.
///
/// Both arms are named because the point is that the surface never leaves the
/// contract: a served operation carries a binding, a contract, and one of the
/// three outcome families; a refused one carries the safe problem record and a
/// status that says so. Anything else — a bare `405`, an empty body, an
/// untagged object — is the failure this gate exists to catch.
fn assert_canonical_envelope(label: &str, status: u16, body: &Value) {
    assert_ne!(status, 404, "{label} must be mounted: {body}");
    assert_ne!(status, 405, "{label} must accept POST: {body}");
    let kind = body["kind"]
        .as_str()
        .unwrap_or_else(|| panic!("{label} must answer the canonical envelope: {body}"));
    match kind {
        "success" => {
            assert_eq!(status, 200, "{label} success must be 200: {body}");
            let outcome = body["value"]["outcome"]["outcome"]
                .as_str()
                .unwrap_or_else(|| panic!("{label} success must name an outcome family: {body}"));
            assert!(
                matches!(outcome, "evidence" | "preview" | "effect"),
                "{label} answered an unknown outcome family `{outcome}`: {body}"
            );
            assert!(
                body["value"]["binding_id"].is_string(),
                "{label} success must name its catalog binding: {body}"
            );
            assert!(
                body["value"]["contract"]["schema_id"].is_string(),
                "{label} success must carry its result contract: {body}"
            );
            assert!(
                body["value"]["scope"]["project_id"].is_string(),
                "{label} success must carry the resolved scope: {body}"
            );
        }
        "problem" => {
            assert!(
                status >= 400,
                "{label} problem must not be reported as success: {body}"
            );
            assert!(
                body["value"]["problem"]["kind"].is_string(),
                "{label} problem must carry the safe problem record: {body}"
            );
            assert!(
                body["value"]["problem"]["retry"].is_string(),
                "{label} problem must state a retry directive: {body}"
            );
        }
        other => panic!("{label} answered an unknown envelope kind `{other}`: {body}"),
    }
}

fn post_envelope(
    agent: &ureq::Agent,
    url: &str,
    fixture: &ProductionDaemon,
    body: &Value,
) -> (u16, Value) {
    let mut response = agent
        .post(url)
        .header("authorization", &fixture.authorization)
        .header("origin", &fixture.origin)
        .content_type("application/json")
        .send(body.to_string())
        .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("POST {url} body failed: {error}"));
    let parsed = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("POST {url} answered non-JSON `{text}`: {error}"));
    (status, parsed)
}

/// Posts to the dashboard's public mount, which needs no daemon credentials of
/// its own: it resolves the active project and forwards through the same owner.
fn post_dashboard_envelope(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let mut response = agent
        .post(url)
        .content_type("application/json")
        .send(body.to_string())
        .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("POST {url} body failed: {error}"));
    let parsed = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("POST {url} answered non-JSON `{text}`: {error}"));
    (status, parsed)
}

/// Polls one Work read until the per-project runtime leaves its mounting
/// window, holding every pre-terminal answer to the typed warming contract.
///
/// The mounting window between daemon start and the project runtime binding is
/// a real production state the dashboard renders as retryable warming, so it
/// is graded here rather than slept past: every answer inside the window must
/// be the typed retryable unavailable problem — never an empty success, a
/// crash, or a concealment — and the first answer outside it is returned for
/// the caller's strict assertions.
fn poll_past_warming(label: &str, post: &mut dyn FnMut() -> (u16, Value)) -> (u16, Value) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (status, body) = post();
        assert_canonical_envelope(label, status, &body);
        if status != 503 {
            return (status, body);
        }
        let problem = &body["value"]["problem"];
        assert_eq!(
            problem["kind"], "unavailable",
            "{label} may only answer the typed unavailable problem while warming: {body}"
        );
        assert_eq!(
            problem["retryable"], true,
            "{label} warming answer must be retryable: {body}"
        );
        assert!(
            Instant::now() < deadline,
            "{label} never left the warming state: {body}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Grades one refused answer: the canonical problem envelope, the exact typed
/// problem kind, the HTTP status that kind maps to, and its retry disposition.
///
/// This deliberately does not reuse [`assert_canonical_envelope`]: that helper
/// reads any `404` as an unmounted route, but a concealed denial answers `404`
/// *with* the canonical problem record — the discriminator between the two is
/// the envelope in the body, which is exactly what is asserted here.
fn assert_typed_problem(label: &str, status: u16, body: &Value, expected: (u16, &str, bool)) {
    let (expected_status, expected_kind, expected_retryable) = expected;
    assert_eq!(status, expected_status, "{label}: {body}");
    assert_eq!(
        body["kind"], "problem",
        "{label} must answer the canonical problem envelope: {body}"
    );
    let problem = &body["value"]["problem"];
    assert_eq!(problem["kind"], expected_kind, "{label}: {body}");
    assert_eq!(problem["retryable"], expected_retryable, "{label}: {body}");
    assert!(
        problem["retry"].is_string(),
        "{label} problem must state a retry directive: {body}"
    );
}

/// The Work surface answers real requests, on both surfaces that publish it.
///
/// The dashboard Work workspace binds `/api/work/*`; the SDKs and hosts call the
/// daemon's `/application/work/*`. `work_api.rs` states that these are the same
/// handler, owner, and dispatch "one segment of path apart", and nothing proved
/// it: every previous test of the surface either stubbed the owner, mocked the
/// fetch, or graded route registration without looking at the answer. This runs
/// both surfaces of one live daemon in the order a real client encounters them
/// and grades every typed state the dashboard renders — warming, absence,
/// staleness, denial, refusal, and the real payloads — so a drift on either
/// side is a failure with a named side.
#[test]
fn the_work_surface_answers_real_requests_on_both_published_mounts() {
    let fixture = ProductionDaemon::start();
    let dashboard = DashboardProcess::start(&fixture);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();

    let list_request = serde_json::json!({ "page_size": 25, "cursor": null });
    let daemon_list = fixture.external_url("/application/work/list-attempts");
    let dashboard_list = format!("{}/api/work/list-attempts", dashboard.base_url);

    // -- Warming, then typed absence: a scope with no Work at all. -----------
    // An empty authorized scope is the explicit `absent` state, not an empty
    // page and not a concealment; a fabricated empty list here is exactly what
    // the typed state exists to prevent.
    for (label, post) in [
        (
            "daemon work/list-attempts (empty scope)",
            &mut (|| post_envelope(&agent, &daemon_list, &fixture, &list_request))
                as &mut dyn FnMut() -> (u16, Value),
        ),
        (
            "dashboard api/work/list-attempts (empty scope)",
            &mut || post_dashboard_envelope(&agent, &dashboard_list, &list_request),
        ),
    ] {
        let (status, body) = poll_past_warming(label, post);
        eprintln!("{label} -> {status} {body}");
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["value"]["outcome"]["outcome"], "evidence", "{body}");
        assert_eq!(
            body["value"]["outcome"]["value"]["payload"]["state"], "absent",
            "{label} must answer the typed absent state for a scope with no Work: {body}"
        );
    }

    // -- Staleness against an absent topology. -------------------------------
    // A cursor names the snapshot it was minted under; with no topology in
    // scope, resuming would fabricate a page, so the answer is the typed stale
    // refusal that tells the client to restart from the first page.
    let stale = superseded_cursor_request();
    for (label, (status, body)) in [
        (
            "daemon work/list-attempts (cursor without topology)",
            post_envelope(&agent, &daemon_list, &fixture, &stale),
        ),
        (
            "dashboard api/work/list-attempts (cursor without topology)",
            post_dashboard_envelope(&agent, &dashboard_list, &stale),
        ),
    ] {
        eprintln!("{label} -> {status} {body}");
        assert_typed_problem(label, status, &body, (409, "stale", true));
    }

    // -- Denial: an attempt that does not exist is concealed. ----------------
    // Absence and denial share one shape by design, so probing an identity
    // cannot reveal whether it exists for someone else.
    let missing_attempt = serde_json::json!({
        "task_id": "task.work-surface-conformance",
        "run_id": "run.work-surface-conformance",
        "attempt_id": "attempt.work-surface-conformance.1",
    });
    let daemon_status = fixture.external_url("/application/work/attempt-status");
    let dashboard_status = format!("{}/api/work/attempt-status", dashboard.base_url);
    for (label, (status, body)) in [
        (
            "daemon work/attempt-status (missing attempt)",
            post_envelope(&agent, &daemon_status, &fixture, &missing_attempt),
        ),
        (
            "dashboard api/work/attempt-status (missing attempt)",
            post_dashboard_envelope(&agent, &dashboard_status, &missing_attempt),
        ),
    ] {
        eprintln!("{label} -> {status} {body}");
        assert_typed_problem(
            label,
            status,
            &body,
            (404, "not_found_or_not_authorized", false),
        );
    }

    // -- Refusals the typed request contract makes before dispatch. ----------
    // An out-of-bounds page size decodes but is refused by the operation; a
    // body the contract cannot decode is refused before the operation ever
    // observes it. Both are client refusals, never server faults.
    for (label, request) in [
        (
            "daemon work/list-attempts (zero page size)",
            serde_json::json!({ "page_size": 0 }),
        ),
        (
            "daemon work/list-attempts (page size above the cap)",
            serde_json::json!({ "page_size": 1_001 }),
        ),
    ] {
        let (status, body) = post_envelope(&agent, &daemon_list, &fixture, &request);
        eprintln!("{label} -> {status} {body}");
        assert_typed_problem(label, status, &body, (400, "invalid_request", false));
    }
    let mut malformed_views = current_product_graph_request(1_700_000_000_000_100);
    malformed_views["observed_at"] = serde_json::json!("not-a-number");
    let (status, body) = post_envelope(
        &agent,
        &fixture.external_url("/application/work/views"),
        &fixture,
        &malformed_views,
    );
    eprintln!("DAEMON work/views malformed -> {status} {body}");
    assert_eq!(
        body["kind"], "problem",
        "a malformed body is refused: {body}"
    );
    assert!(
        (400..500).contains(&status),
        "a malformed body is a client refusal, not a server fault: {status} {body}"
    );

    // An operation this build does not mount is concealed exactly like an
    // unauthorised one, so probing a path cannot reveal what exists.
    let (status, body) = post_envelope(
        &agent,
        &fixture.external_url("/application/work/not-an-operation"),
        &fixture,
        &serde_json::json!({}),
    );
    eprintln!("DAEMON work/not-an-operation -> {status} {body}");
    assert_eq!(status, 404, "an unmounted Work segment is refused: {body}");

    // -- Real payload: prepare and commit through the daemon, then read the
    // exact product graph through both mounts. Preparation is the authority
    // handoff: the caller never fabricates graph or revision CAS pins.
    let (status, prepared) = post_envelope(
        &agent,
        &fixture.external_url("/application/work/prepare-graph-mutation"),
        &fixture,
        &product_task_create_draft(),
    );
    eprintln!("DAEMON work/prepare-graph-mutation -> {status} {prepared}");
    assert_canonical_envelope("daemon work/prepare-graph-mutation", status, &prepared);
    assert_eq!(prepared["value"]["outcome"]["outcome"], "evidence");
    let mutation = prepared["value"]["outcome"]["value"]["payload"].clone();
    assert_eq!(mutation["mutation"], "create_task", "{prepared}");

    let (status, created) = post_envelope(
        &agent,
        &fixture.external_url("/application/work/mutate-graph"),
        &fixture,
        &mutation,
    );
    eprintln!("DAEMON work/mutate-graph -> {status} {created}");
    assert_canonical_envelope("daemon work/mutate-graph", status, &created);
    let effect = &created["value"]["outcome"]["value"];
    assert_eq!(
        created["value"]["outcome"]["outcome"], "effect",
        "{created}"
    );
    assert_eq!(effect["reconciliation"], "reconciled", "{created}");
    assert_eq!(effect["receipt"]["outcome"], "completed", "{created}");
    assert_eq!(
        effect["payload"]["event"]["payload"]["kind"], "created",
        "{created}"
    );

    let observed_at = effect["payload"]["event"]["occurred_at"]
        .as_i64()
        .expect("created event observation time");
    work_evidence::assert_live_task_rooted_retrieval(&agent, &fixture, &dashboard, observed_at);

    for retired in ["snapshot", "delta", "replan-dependencies", "accept-task"] {
        let (status, body) = post_envelope(
            &agent,
            &fixture.external_url(&format!("/application/work/{retired}")),
            &fixture,
            &serde_json::json!({}),
        );
        assert_eq!(status, 404, "retired daemon Work route {retired}: {body}");
        let (status, body) = post_dashboard_envelope(
            &agent,
            &format!("{}/api/work/{retired}", dashboard.base_url),
            &serde_json::json!({}),
        );
        assert_eq!(
            status, 404,
            "retired dashboard Work route {retired}: {body}"
        );
    }

    // -- Product publication does not fabricate executor topology. -----------
    // Product graph state and executor topology have distinct authorities. A
    // committed task is readable through Work views, but cannot by itself mint
    // a topology generation or an authorized empty attempts page.
    for (label, post) in [
        (
            "daemon work/list-attempts (product graph only)",
            &mut (|| post_envelope(&agent, &daemon_list, &fixture, &list_request))
                as &mut dyn FnMut() -> (u16, Value),
        ),
        (
            "dashboard api/work/list-attempts (product graph only)",
            &mut || post_dashboard_envelope(&agent, &dashboard_list, &list_request),
        ),
    ] {
        let (status, body) = poll_past_warming(label, post);
        eprintln!("{label} -> {status} {body}");
        assert_eq!(status, 200, "{label}: {body}");
        assert_eq!(body["value"]["outcome"]["outcome"], "evidence", "{body}");
        let payload = &body["value"]["outcome"]["value"]["payload"];
        assert_eq!(
            payload["state"], "absent",
            "{label} must not alias product graph tasks into executor topology: {body}"
        );
    }
}

/// The dashboard Work journey past its verified task root.
///
/// `the_work_surface_answers_real_requests_on_both_published_mounts` proves the
/// TaskId-rooted read at its floor: a task with no accepted attempt, whose only
/// truthful answer is zero selected sources. The question a dashboard user
/// actually opens a task to ask — *who worked on this, and in which provider
/// session* — was never driven on either mount. This runs one real pinned
/// provider through the production spawn path, links the accepted attempt,
/// imports the provider transcript, and grades the answer on the daemon mount
/// and the dashboard mount, in all four temporal modes, across a physical
/// daemon restart.
#[cfg(unix)]
#[test]
fn the_dashboard_work_surface_answers_who_worked_on_a_task_on_both_published_mounts() {
    let mut fixture = ProductionDaemon::start();
    let mut dashboard = DashboardProcess::start(&fixture);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();

    work_task_session::assert_provider_qualified_task_session_evidence(
        &agent,
        &mut fixture,
        &mut dashboard,
    );
}

#[test]
fn public_executable_routes_are_served_by_the_production_daemon() {
    let fixture = ProductionDaemon::start();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();

    assert_external_surface_is_authenticated_and_resolving(&agent, &fixture);

    let registry =
        work_executable_binding_registry().expect("canonical application binding registry");
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
    // A withheld entry carries no route path, so it would silently shrink the
    // set under test rather than be reported as unreachable.
    assert!(
        withheld.is_empty(),
        "the canonical registry withheld {} executable binding(s), so the route \
         probes below would grade a silently truncated set: {}",
        withheld.len(),
        withheld.join(", ")
    );
    assert!(
        !declared_routes.is_empty(),
        "the canonical registry advertised no public executable routes, so this \
         contract would pass vacuously"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    let inner = runtime.block_on(inner_router(&fixture.project));

    let mut observations = Vec::with_capacity(declared_routes.len());
    for (operation_id, (route_path, body)) in declared_routes {
        let external_url = fixture.external_url(&route_path);
        let inner_path = route_path
            .strip_prefix("/application")
            .unwrap_or(route_path.as_str());
        observations.push(RouteObservation {
            outer_method_mismatch: get_probe(&agent, &external_url, &fixture),
            outer_request: post_probe(&agent, &external_url, &fixture, &body),
            inner_method_mismatch: runtime.block_on(inner_probe(&inner, "GET", inner_path, None)),
            inner_request: runtime.block_on(inner_probe(&inner, "POST", inner_path, Some(body))),
            operation_id,
            route_path,
            external_url,
        });
    }

    for observation in &observations {
        for (surface, probe) in [
            ("external", observation.outer_method_mismatch),
            ("inner", observation.inner_method_mismatch),
        ] {
            assert!(
                probe.status == StatusCode::NOT_FOUND.as_u16()
                    || probe.status == StatusCode::METHOD_NOT_ALLOWED.as_u16(),
                "the {surface} method-mismatch probe answered {} — neither 404 \
                 nor 405 — so it no longer discriminates a mounted path. A \
                 binding served on GET as well as POST would do this; give such \
                 a binding a probe method it does not serve instead of relaxing \
                 this check.\n  {}",
                probe.status,
                observation.describe()
            );
        }
        assert_eq!(
            observation.outer_method_mismatch.path_is_registered(),
            observation.outer_request.reached_a_handler(),
            "the external routing-table and representative-request signals \
             disagree, so neither can be trusted:\n  {}",
            observation.describe()
        );
        assert_eq!(
            observation.inner_method_mismatch.path_is_registered(),
            observation.inner_request.reached_a_handler(),
            "the inner routing-table and representative-request signals \
             disagree, so neither can be trusted:\n  {}",
            observation.describe()
        );
    }

    let missing = observations
        .iter()
        .filter(|observation| !observation.outer_method_mismatch.path_is_registered())
        .collect::<Vec<_>>();
    // A route the inner router serves but the external surface does not is a
    // prefix defect: the inner router registered an absolute path that the
    // outer `{*tail}` rewrite has already stripped. Failing on disagreement
    // alone is what stops such a regression from hiding behind inner evidence.
    let disagreements = observations
        .iter()
        .filter(|observation| {
            observation.inner_method_mismatch.path_is_registered()
                != observation.outer_method_mismatch.path_is_registered()
        })
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty() && disagreements.is_empty(),
        "{} of {} canonical public executable routes are advertised by the \
         catalog but are not reachable on the live daemon's HTTP application \
         endpoint, and {} disagree between the external surface and the \
         in-process router.\n\nunreachable externally:\n{}\n\ninner/outer \
         disagreement (the in-process router serves it but the daemon does not, \
         so the inner router registered a path the outer \
         /projects/{{id}}/application/{{*tail}} rewrite already \
         strips):\n{}\n\nreachable externally:\n{}",
        missing.len(),
        observations.len(),
        disagreements.len(),
        describe_all(&missing),
        describe_all(&disagreements),
        describe_all(
            &observations
                .iter()
                .filter(|observation| observation.outer_method_mismatch.path_is_registered())
                .collect::<Vec<_>>()
        ),
    );
}

fn describe_all(observations: &[&RouteObservation]) -> String {
    if observations.is_empty() {
        return "  (none)".to_owned();
    }
    observations
        .iter()
        .map(|observation| format!("  {}", observation.describe()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Establishes that the external surface enforces an authenticated local origin
/// and that a `404` there really means the route is absent.
///
/// Without these preconditions an external verdict is meaningless: an
/// unauthenticated endpoint would answer `401` everywhere, and an unresolved
/// project makes `dispatch_project_application` answer `404` for every path, so
/// every route would score as missing for the wrong reason.
fn assert_external_surface_is_authenticated_and_resolving(
    agent: &ureq::Agent,
    fixture: &ProductionDaemon,
) {
    let witness = fixture.external_url(RELATIVE_WITNESS_TAIL);

    let anonymous = agent
        .get(witness.as_str())
        .header("origin", &fixture.origin)
        .call()
        .expect("anonymous probe response");
    assert_eq!(
        anonymous.status().as_u16(),
        StatusCode::UNAUTHORIZED.as_u16(),
        "the daemon HTTP application endpoint served a request with no bearer \
         token, so these probes would not be proving an authenticated surface"
    );

    let foreign_origin = agent
        .get(witness.as_str())
        .header("authorization", &fixture.authorization)
        .header("origin", "http://route-exposure-conformance.invalid")
        .call()
        .expect("foreign-origin probe response");
    assert_eq!(
        foreign_origin.status().as_u16(),
        StatusCode::FORBIDDEN.as_u16(),
        "the daemon HTTP application endpoint accepted a foreign origin, so \
         these probes would not be proving a local-origin surface"
    );

    let resolved = get_probe(agent, &witness, fixture);
    assert_eq!(
        resolved.status,
        StatusCode::METHOD_NOT_ALLOWED.as_u16(),
        "{witness} is a POST route that tracedecay_api::application_router \
         registers relative to the outer prefix, so an authenticated GET must \
         answer 405. Got {} instead, which means the outer dispatch never \
         resolved the project or never reached the inner routing table, and no \
         per-route verdict below would be trustworthy.",
        resolved.status
    );

    let absent = get_probe(agent, &fixture.external_url(ABSENT_TAIL), fixture);
    assert_eq!(
        absent.status,
        StatusCode::NOT_FOUND.as_u16(),
        "an undeclared tail must answer 404, otherwise no probe can distinguish \
         a mounted handler from a catch-all"
    );
    assert_eq!(
        absent.body_len, 0,
        "the router fallback must answer 404 with an empty body for the \
         body-length signal to separate a fallback from a handler problem"
    );

    let unknown_project = format!(
        "{}/projects/{UNKNOWN_PROJECT_ID}{RELATIVE_WITNESS_TAIL}",
        fixture.base_url
    );
    let unresolved = get_probe(agent, &unknown_project, fixture);
    assert_eq!(
        unresolved.status,
        StatusCode::NOT_FOUND.as_u16(),
        "an unresolvable project must answer 404, which is exactly why the \
         witness probe above has to pass before any route is called missing"
    );
}

fn get_probe(agent: &ureq::Agent, url: &str, fixture: &ProductionDaemon) -> ProbeResult {
    let mut response = agent
        .get(url)
        .header("authorization", &fixture.authorization)
        .header("origin", &fixture.origin)
        .call()
        .unwrap_or_else(|error| panic!("GET {url} failed: {error}"));
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("GET {url} body failed: {error}"));
    ProbeResult {
        status,
        body_len: body.len(),
    }
}

fn post_probe(
    agent: &ureq::Agent,
    url: &str,
    fixture: &ProductionDaemon,
    body: &Value,
) -> ProbeResult {
    let mut response = agent
        .post(url)
        .header("authorization", &fixture.authorization)
        .header("origin", &fixture.origin)
        .content_type("application/json")
        .send(body.to_string())
        .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("POST {url} body failed: {error}"));
    ProbeResult {
        status,
        body_len: body.len(),
    }
}

/// Builds the same in-process router the daemon mounts, used only as secondary
/// evidence so an inner/outer disagreement can be named precisely.
async fn inner_router(project: &Path) -> axum::Router {
    let handshake =
        DaemonHandshake::for_current_client(Some(project.to_path_buf()), None, false, false)
            .expect("production daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake).expect("production daemon client");
    http_application_router(
        client,
        OperationEventAuthority::default(),
        ProjectId::new("project.route-exposure-conformance").expect("conformance project identity"),
    )
    .expect("production merged HTTP application router")
}

async fn inner_probe(
    router: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> ProbeResult {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(match &body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        })
        .expect("inner probe request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("inner probe response");
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("inner probe body");
    ProbeResult {
        status,
        body_len: bytes.len(),
    }
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
