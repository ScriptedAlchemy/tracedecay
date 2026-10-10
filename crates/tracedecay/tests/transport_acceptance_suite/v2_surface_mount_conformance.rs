//! Mount conformance for every declared product surface, in both directions.
//!
//! "All features must be properly mounted" is only provable by walking the two
//! directions separately, because each one misses a different defect:
//!
//! * **Forward** (catalog -> surface): every binding the runtime catalog
//!   snapshot declares must be reachable on the surface it declares itself on.
//!   This catches a capability that was contributed to the catalog but whose
//!   adapter never registered a route, a tool, or a command. It cannot catch a
//!   feature that never reached the catalog at all.
//! * **Reverse** (operation enum -> catalog -> surface): every operation family
//!   that exists *independently* of the catalog, the Work and Workflow
//!   operation enums, the HTTP application operation enum, the callable-code
//!   operation set, the activity families, the Work product read model, must
//!   be catalog-declared **and** mounted, or listed in
//!   [`SANCTIONED_UNMOUNTED`] with the plan that sanctions the absence. This is
//!   the direction that flushes out unmounted surfaces, and it is the reason
//!   this suite exists: an operation that no adapter ever bound is invisible to
//!   a forward-only sweep.
//!
//! Two rules keep this from decaying into a suite that passes vacuously:
//!
//! 1. **No silent skips.** An unknown or unresolvable subject is a failure, not
//!    a `continue`. Every enumeration asserts it found a non-empty set before
//!    grading it, and every unmatched subject is collected into a failure
//!    inventory that names the exact missing mount.
//! 2. **Absences are typed, never absent.** A denied or unavailable surface
//!    still answers its canonical envelope; only a genuinely unregistered path
//!    answers the router's empty-bodied `404`. Route verdicts are therefore
//!    taken from axum's routing table (a `GET` to a `POST`-only route answers
//!    `405` when the path is registered and `404` when it is not), which is
//!    immune to handler semantics, a mounted handler is allowed to conceal a
//!    denial as `404` and must not be scored as unmounted for it.
//!
//! Surfaces are driven the way a client drives them: the live daemon's
//! published HTTP application endpoint, the real `tracedecay serve` MCP server
//! over stdio, the real `tracedecay tool` command listing, a Content-Length
//! framed `tracedecay lsp bridge` session, and a live `tracedecay dashboard`
//! server. Nothing here inspects source files or shells out to a scanner; each
//! assertion is a product call.

use crate::common;
use crate::common::run_ok;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tracedecay_api::{
    WorkOperation, WorkflowOperation, http_application_full_route_path,
    is_http_application_operation_exposed,
};
use tracedecay_contracts::catalog_composition::build_application_catalog_snapshot;
use tracedecay_runtime_core::ast_grep::{AST_GREP_BIN_ENV, ast_grep_command};
use tracedecay_session_memory::event_lane::ActivityFamilyV1;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CapabilityManifestV1, CatalogSnapshotV1,
    FeatureId, SurfaceOperationName,
};

/// Absences the plan set sanctions, as `(subject, plan citation)`.
///
/// A subject listed here is *known* to have no mount and is exempted from the
/// sweeps below. Everything else that is unmounted fails. The citation is the
/// plan text that sanctions the absence; an entry without one does not belong
/// here. The whole table is printed alongside every failure so the next reader
/// can tell a sanctioned gap from a regression without leaving the output.
///
/// Subjects are spelled `surface:name` so a mount landing on one surface but
/// not another stays individually accountable.
///
/// The table is empty: every declared surface must be mounted. The const
/// stays so the next genuinely sanctioned gap fails loudly until it is
/// written down here with its plan citation.
const SANCTIONED_UNMOUNTED: &[(&str, &str)] = &[];

/// The callable-code operations, mirroring the set that
/// `http_page_projection` classifies as `HttpPageProjection::MetaCursor` in
/// `tracedecay-daemon-service/src/application_surface.rs`.
///
/// These are the operations whose decoded surface request carries callable-code
/// metadata. CLI, MCP, and HTTP must expose the same application operations.
/// Restating the set here is deliberate: it is the reverse authority, so it
/// must not be derived from the thing under test.
const CALLABLE_CODE_OPERATIONS: [ApplicationSurfaceOperation; 13] = [
    ApplicationSurfaceOperation::CodeExactOccurrence,
    ApplicationSurfaceOperation::CodePhraseSearch,
    ApplicationSurfaceOperation::CodeSymbolSearch,
    ApplicationSurfaceOperation::CodeSignatureSearch,
    ApplicationSurfaceOperation::CodeImplementations,
    ApplicationSurfaceOperation::CodeTypeHierarchy,
    ApplicationSurfaceOperation::CodeCallers,
    ApplicationSurfaceOperation::CodeCallees,
    ApplicationSurfaceOperation::CodeFacets,
    ApplicationSurfaceOperation::CodeTimeline,
    ApplicationSurfaceOperation::CodeDeclaration,
    ApplicationSurfaceOperation::CodeTypeDefinition,
    ApplicationSurfaceOperation::CodeReferences,
];

/// Tail no binding declares, used to prove the daemon really answers `404` for
/// an unregistered path instead of swallowing everything into a catch-all.
const ABSENT_TAIL: &str = "/application/surface-mount-conformance-absent";

/// A `POST` route the application router registers relative to the outer
/// project prefix. Reaching it proves the outer dispatch resolved the project,
/// applied the `{*tail}` rewrite, and handed off to the inner routing table,
/// without which every route below would score as missing for the wrong reason.
const RELATIVE_WITNESS_TAIL: &str = "/application/primitives/storage_status";

fn sanctioned_citation(subject: &str) -> Option<&'static str> {
    SANCTIONED_UNMOUNTED
        .iter()
        .find(|(name, _)| *name == subject)
        .map(|(_, citation)| *citation)
}

fn sanctioned_table() -> String {
    SANCTIONED_UNMOUNTED
        .iter()
        .map(|(subject, citation)| format!("  {subject}\n      {citation}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a failure inventory with the sanctioned table appended, so the
/// message names both what is missing and what was already allowed to be.
fn report(headline: &str, failures: &[String]) -> String {
    format!(
        "{headline}\n\nunmounted ({}):\n{}\n\nsanctioned absences ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .map(|failure| format!("  {failure}"))
            .collect::<Vec<_>>()
            .join("\n"),
        SANCTIONED_UNMOUNTED.len(),
        sanctioned_table(),
    )
}

/// A live daemon over one registered project inside a throwaway profile, plus
/// the credentials it published for its own HTTP application endpoint.
///
/// Every command this fixture runs carries the isolated home on its own
/// environment; the test process environment is never mutated, so the fixture
/// cannot leak into a developer profile or into a concurrently running suite.
struct MountFixture {
    home: PathBuf,
    project: PathBuf,
    project_id: String,
    base_url: String,
    origin: String,
    authorization: String,
    // Field order is drop order: the daemon child must be reaped before the
    // temporary home it writes into is removed.
    _daemon: common::DaemonProcess,
    _home_dir: TempDir,
}

impl MountFixture {
    fn start() -> Self {
        let home_dir = common::tempdir_or_panic();
        let home = home_dir.path().to_path_buf();
        let profile = home.join(".tracedecay");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile)
            .expect("isolated profile root");
        let project = home.join("project #mount");
        fs::create_dir_all(project.join("src")).expect("isolated project root");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname=\"surface-mount-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
        )
        .expect("fixture manifest");
        fs::write(
            project.join("src/lib.rs"),
            "pub const SURFACE_MOUNT_FIXTURE: bool = true;\n",
        )
        .expect("fixture source");

        run_ok(
            Command::new(common::git_program())
                .args(["init", "--quiet"])
                .current_dir(&project),
            "git init",
        );
        common::initialize_tracedecay_cli_project(&home, &project);

        let daemon = common::spawn_tracedecay_daemon_with(&home, |command| {
            command.env(AST_GREP_BIN_ENV, ast_grep_command().get_program());
        });
        let authority = wait_for_http_authority(&common::daemon_authority_path(&profile));

        let context = run_ok(
            isolated_command(&home)
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
            home,
            project,
            project_id,
            base_url: format!("http://{endpoint}"),
            origin: format!("http://{endpoint}"),
            authorization: format!("Bearer {token}"),
            _daemon: daemon,
            _home_dir: home_dir,
        }
    }

    /// External URL for a canonical application route path, which already
    /// starts with `/application` and composes onto the project prefix.
    fn external_url(&self, route_path: &str) -> String {
        format!(
            "{}/projects/{}{}",
            self.base_url, self.project_id, route_path
        )
    }
}

fn isolated_command(home: &Path) -> Command {
    let mut command = common::tracedecay_command_with_home(home);
    // Isolated children retain only system PATH entries. Pass the executable
    // selected by the shared resolver, including npm's native Windows binary.
    command
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1")
        .env(AST_GREP_BIN_ENV, ast_grep_command().get_program());
    command
}

/// The dashboard server's closed operation->route table, restated in the test
/// so the probe does not derive its routes from the catalog it grades.
///
/// The dashboard adapter mounts three dispatch surfaces: the configuration
/// router at `/api/dashboard/application/configuration/{operation}`, the
/// feedback read router at `/api/feedback/{tail}`, and the native-integration
/// status read under the active-project gateway. A catalog operation claiming
/// the Dashboard surface that none of these tables answers is unmounted.
fn dashboard_operation_route(operation: &str) -> Option<(&'static str, String)> {
    if operation.starts_with("configuration_") {
        return Some((
            "POST",
            format!("/api/dashboard/application/configuration/{operation}"),
        ));
    }
    match operation {
        "feedback_get" => Some(("POST", "/api/feedback/get".to_owned())),
        "feedback_expand" => Some(("POST", "/api/feedback/expand".to_owned())),
        "feedback_list" => Some(("POST", "/api/feedback/list".to_owned())),
        "feedback_proximity" => Some(("POST", "/api/feedback/proximity".to_owned())),
        "native_integration_status" => Some(("GET", "/api/native-integration/status".to_owned())),
        _ => None,
    }
}

/// A live `tracedecay dashboard` server answering for the fixture's project.
///
/// The dashboard mints a per-listener token that the launch URL carries once;
/// programmatic clients replay it as the Basic-auth password, which ureq does
/// when the base URL carries `tracedecay:{token}` userinfo.
struct DashboardProbe {
    agent: ureq::Agent,
    base_url: String,
}

impl DashboardProbe {
    fn start(fixture: &MountFixture) -> Self {
        let stdout = run_ok(
            isolated_command(&fixture.home)
                .args(["dashboard", "--port", "0", "--path"])
                .arg(&fixture.project)
                .current_dir(&fixture.project),
            "tracedecay dashboard",
        );
        let stdout = String::from_utf8_lossy(&stdout);
        let url = stdout
            .split_whitespace()
            .find(|token| token.starts_with("http://"))
            .unwrap_or_else(|| panic!("tracedecay dashboard printed no launch URL:\n{stdout}"))
            .to_owned();
        let without_scheme = url.trim_start_matches("http://");
        let (authority, query) = without_scheme
            .split_once("/?")
            .unwrap_or((without_scheme, ""));
        let token = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .unwrap_or_else(|| panic!("dashboard launch URL carried no access token: {url}"));
        let probe = Self {
            agent: common::http_agent_with_timeout(Duration::from_secs(30)),
            base_url: format!("http://tracedecay:{token}@{authority}"),
        };
        probe.assert_discriminates();
        probe
    }

    fn request_status(&self, method: &str, path: &str) -> u16 {
        let url = format!("{}{path}", self.base_url);
        let response = common::http_call_with_retry(&format!("{method} {url}"), || match method {
            "GET" => self.agent.get(&url).call(),
            _ => self.agent.post(&url).send_json(serde_json::json!({})),
        });
        response.status().as_u16()
    }

    /// Prove the route probes discriminate mounted from absent: a tail no
    /// dashboard dispatch table holds must answer 404, and a known mounted
    /// operation must answer something else. Without these preconditions
    /// every operation could score mounted against a catch-all or unmounted
    /// against a dead server.
    fn assert_discriminates(&self) {
        assert_eq!(
            self.request_status("POST", "/api/feedback/no-such-read"),
            404,
            "the dashboard's feedback router must conceal an unresolvable \
             operation as 404 for the mount probe to discriminate"
        );
        assert_ne!(
            self.request_status(
                "POST",
                "/api/dashboard/application/configuration/configuration_list",
            ),
            404,
            "configuration_list is dispatchable on the dashboard surface; \
             a 404 here means the dashboard application routes never mounted, \
             so no operation verdict below would be trustworthy"
        );
    }

    fn operation_is_mounted(&self, operation: &str) -> bool {
        let Some((method, path)) = dashboard_operation_route(operation) else {
            return false;
        };
        self.request_status(method, &path) != 404
    }
}

/// One Content-Length-framed JSON-RPC reply from the live `lsp bridge`.
fn read_lsp_frame(reader: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let length = content_length?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// A live `tracedecay lsp bridge --stdio` session bound to the fixture's
/// project.
///
/// The bridge forwards each frame to a daemon-owned LSP session. Dispatch
/// gates catalog-routable methods on the session's catalog binding before
/// reaching a handler, so the absence verdict is exact: an operation whose
/// catalog binding the session cannot resolve, or a method the surface
/// explicitly defers, answers `Method unavailable` (-32601) with
/// `explicitlyUnavailable` in the error data. Parameter-parse errors,
/// capability negotiation, and provider unavailability come back with
/// different codes or reasons from the handler layer beneath the gate, which
/// is what marks the operation mounted. Catalog admission rejects unroutable
/// operation names at construction, so every declared binding reaches this
/// gate.
struct LspBridgeProbe {
    _process: common::TestChildProcess,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Value>,
    next_id: i64,
}

impl LspBridgeProbe {
    fn start(fixture: &MountFixture) -> Self {
        let mut child = isolated_command(&fixture.home)
            .args(["lsp", "bridge", "--stdio", "--project"])
            .arg(&fixture.project)
            .current_dir(&fixture.project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tracedecay lsp bridge should start");
        let stdin = child.stdin.take().expect("lsp bridge stdin");
        let mut reader = BufReader::new(child.stdout.take().expect("lsp bridge stdout"));
        if let Some(mut stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            });
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(message) = read_lsp_frame(&mut reader) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        let mut probe = Self {
            _process: common::TestChildProcess::new(child),
            stdin,
            responses: rx,
            next_id: 0,
        };
        let root_uri = url::Url::from_file_path(&fixture.project)
            .expect("fixture project has an absolute file URI")
            .to_string();
        let initialize = probe.request(
            "initialize",
            serde_json::json!({
                "processId": Value::Null,
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "declaration": {},
                        "definition": {},
                        "typeDefinition": {},
                        "implementation": {},
                        "references": {},
                        "hover": {},
                        "documentSymbol": {},
                        "signatureHelp": {},
                        "callHierarchy": {},
                        "typeHierarchy": {},
                        "diagnostic": {},
                    },
                    "workspace": {
                        "symbol": {},
                        "workspaceFolders": true,
                    },
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": "surface-mount-fixture",
                }],
            }),
        );
        assert!(
            initialize["error"].is_null(),
            "the lsp bridge rejected initialize: {initialize}"
        );
        probe.notify("initialized", serde_json::json!({}));
        probe.assert_discriminates();
        probe
    }

    /// Prove the method probes discriminate mounted from absent: a method the
    /// surface does not mount must answer -32601 explicitlyUnavailable, and a
    /// known catalog-routable method must not.
    fn assert_discriminates(&mut self) {
        let deferred = self.request("workspace/executeCommand", serde_json::json!({}));
        assert_eq!(
            (
                deferred["error"]["code"].as_i64(),
                deferred["error"]["data"]["reason"].as_str()
            ),
            (Some(-32601), Some("explicitlyUnavailable")),
            "a method this surface does not mount must answer -32601 \
             explicitlyUnavailable for the lsp probe to discriminate; got \
             {deferred}"
        );
        let context = self.request("tracedecay/context", serde_json::json!({}));
        assert!(
            Self::verdict_is_mounted(&context),
            "tracedecay/context is catalog-bound and dispatched; an unmounted \
             verdict here means the lsp session's catalog admission failed, so \
             no method verdict below would be trustworthy: {context}"
        );
    }

    fn send_frame(&mut self, value: &Value) {
        let body = serde_json::to_vec(value).expect("lsp frame json");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .and_then(|()| self.stdin.write_all(&body))
            .and_then(|()| self.stdin.flush())
            .expect("write lsp frame");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send_frame(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "the lsp bridge never answered method {method}"
            );
            let message = self
                .responses
                .recv_timeout(remaining)
                .expect("lsp bridge stdout closed before answering");
            if message["id"].as_i64() == Some(id) {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send_frame(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// A response proves the request reached a dispatched handler unless it is
    /// a catalog-admission miss. Catalog-admission rejects unroutable LSP
    /// operation names at construction, so a binding whose method string left
    /// the client table turns the whole session catalog unavailable and every
    /// probe below answers `explicitlyUnavailable`; negotiation- and
    /// provider-level unavailability (`capabilityNotNegotiated`,
    /// `providerUnavailable`, `denied`, root-scoping) is truthful
    /// per-deployment availability of a mounted method, not a mount gap.
    fn verdict_is_mounted(response: &Value) -> bool {
        response["error"]["code"].as_i64() != Some(-32601)
            || response["error"]["data"]["reason"].as_str() != Some("explicitlyUnavailable")
    }

    fn operation_is_mounted(&mut self, operation: &str) -> bool {
        Self::verdict_is_mounted(&self.request(operation, serde_json::json!({})))
    }
}

fn wait_for_http_authority(path: &Path) -> Value {
    common::poll_until(
        Instant::now() + Duration::from_secs(90),
        Duration::from_millis(50),
        || {
            let bytes = fs::read(path).ok()?;
            let record: Value = serde_json::from_slice(&bytes).ok()?;
            let has_token = record["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64);
            let has_endpoint = record["http_application_endpoint"].as_str().is_some();
            (has_token && has_endpoint).then_some(record)
        },
        || {
            format!(
                "timed out waiting for a published HTTP application endpoint at {}",
                path.display()
            )
        },
    )
}

/// Whether the live daemon's routing table holds `route_path`.
///
/// Every canonical application route is served as `POST`, so a `GET` answers
/// `405` exactly when the path is registered and `404` when it is not. The
/// verdict is therefore axum's, not the handler's, which is what keeps a
/// mounted handler's own `404` concealment from reading as an absent route.
fn http_route_is_mounted(agent: &ureq::Agent, fixture: &MountFixture, route_path: &str) -> bool {
    let url = fixture.external_url(route_path);
    let response = common::http_call_with_retry(&format!("GET {url}"), || {
        agent
            .get(url.as_str())
            .header("authorization", &fixture.authorization)
            .header("origin", &fixture.origin)
            .call()
    });
    let status = response.status().as_u16();
    assert!(
        status == 404 || status == 405,
        "the method-mismatch probe for {route_path} answered {status}, neither \
         404 nor 405, so it no longer discriminates a mounted path. A binding \
         served on GET as well as POST would do this; give such a binding a \
         probe method it does not serve rather than relaxing this check."
    );
    status == 405
}

/// Establishes that a `404` from the external surface really means "absent".
///
/// Without these preconditions every route below would score as missing for the
/// wrong reason: an unauthenticated endpoint answers `401` everywhere, and an
/// unresolved project makes the outer dispatch answer `404` for every path.
fn assert_external_surface_discriminates(agent: &ureq::Agent, fixture: &MountFixture) {
    let witness = fixture.external_url(RELATIVE_WITNESS_TAIL);
    let anonymous = common::http_call_with_retry("anonymous witness probe", || {
        agent
            .get(witness.as_str())
            .header("origin", &fixture.origin)
            .call()
    });
    assert_eq!(
        anonymous.status().as_u16(),
        401,
        "the daemon HTTP application endpoint served a request with no bearer \
         token, so these probes would not be proving an authenticated surface"
    );

    let foreign = common::http_call_with_retry("foreign-origin witness probe", || {
        agent
            .get(witness.as_str())
            .header("authorization", &fixture.authorization)
            .header("origin", "http://surface-mount-conformance.invalid")
            .call()
    });
    assert_eq!(
        foreign.status().as_u16(),
        403,
        "the daemon HTTP application endpoint accepted a foreign origin, so \
         these probes would not be proving a local-origin surface"
    );

    assert!(
        http_route_is_mounted(agent, fixture, RELATIVE_WITNESS_TAIL),
        "{RELATIVE_WITNESS_TAIL} is a POST route the application router \
         registers relative to the outer prefix, so an authenticated GET must \
         answer 405. It did not, which means the outer dispatch never resolved \
         the project or never reached the inner routing table, and no \
         per-route verdict below would be trustworthy."
    );
    assert!(
        !http_route_is_mounted(agent, fixture, ABSENT_TAIL),
        "an undeclared tail must answer 404, otherwise no probe can \
         distinguish a mounted handler from a catch-all"
    );
}

/// The tool names the real `tracedecay tool` command publishes.
///
/// This is the CLI's own listing, the same one an operator reads before
/// calling `tracedecay tool <name>`, so a catalog binding missing from it is
/// a binding no CLI user can reach.
fn cli_tool_listing(fixture: &MountFixture) -> BTreeSet<String> {
    let stdout = run_ok(
        isolated_command(&fixture.home)
            .arg("tool")
            .current_dir(&fixture.project),
        "tracedecay tool (CLI listing)",
    );
    let listing = String::from_utf8_lossy(&stdout);
    let names = listing
        .lines()
        .filter_map(|line| {
            // Entries are indented under a bracketed group heading; headings,
            // the banner, and its wrapped continuation lines are not indented.
            let rest = line.strip_prefix("  ")?;
            let name = rest.split_whitespace().next()?;
            (!name.starts_with('[')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
            .then(|| name.to_owned())
        })
        .collect::<BTreeSet<_>>();
    assert!(
        names.len() > 20,
        "the CLI tool listing published only {} name(s), so grading catalog \
         bindings against it would be near-vacuous:\n{listing}",
        names.len()
    );
    names
}

/// The tool names the real MCP server publishes to a client.
///
/// `tracedecay serve` is the stdio MCP transport hosts connect to; it proxies
/// to the same live daemon, and its `tools/list` answer is the catalog-filtered
/// discovery result. Driving it end to end is the only way to prove an MCP
/// binding is discoverable rather than merely declared. `--all-tools` lists
/// the full session catalog rather than the core set.
fn mcp_tool_listing(fixture: &MountFixture) -> BTreeSet<String> {
    let mut child = isolated_command(&fixture.home)
        .args(["serve", "--all-tools"])
        .current_dir(&fixture.project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tracedecay serve should start");
    {
        let stdin = child.stdin.as_mut().expect("serve stdin");
        for line in [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "surface-mount-conformance", "version": "1" },
                },
            }),
            serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        ] {
            writeln!(stdin, "{line}").expect("write MCP request");
        }
    }
    // Closing stdin lets the server finish and exit, which avoids the deadlock
    // an interactive read/write loop would risk on a slow first response.
    let mut process = common::TestChildProcess::new(child);
    let output = process
        .wait_with_output(Duration::from_secs(180))
        .expect("tracedecay serve should answer tools/list and exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let listing = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["id"] == 2)
        .unwrap_or_else(|| {
            panic!("the MCP server never answered tools/list\nstdout:\n{stdout}\nstderr:\n{stderr}")
        });
    let tools = listing["result"]["tools"].as_array().unwrap_or_else(|| {
        panic!("MCP tools/list did not carry a tool array: {listing}\nstderr:\n{stderr}")
    });
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>();
    assert!(
        names.len() > 20,
        "the MCP server published only {} tool(s), so grading catalog bindings \
         against it would be near-vacuous: {listing}",
        names.len()
    );
    names
}

/// The protocol revision every production adapter negotiates
/// (`application_surface::APPLICATION_PROTOCOL_REVISION`), restated here as
/// the reverse authority so a revision bump must update this suite knowingly.
const NEGOTIATED_PROTOCOL_REVISION: u32 = 1;

/// Whether some sanctioned production negotiation resolves this spelling.
///
/// The binding is probed under each profile with exactly its declared
/// required features, the shape of an initialize-time negotiation (none for
/// most surfaces; today the LSP context family declares some). A
/// catalog entry that no profile includes, whose features can never be
/// negotiated, or whose revision range excludes the production protocol still
/// resolves to `None`, which is exactly the "declared but not reachable"
/// state the forward sweep exists to catch.
fn binding_resolves(
    snapshot: &CatalogSnapshotV1,
    surface: BindingSurface,
    operation: &str,
) -> bool {
    let Ok(operation_name) = SurfaceOperationName::new(operation) else {
        return false;
    };
    let Some(binding) = snapshot
        .capabilities()
        .flat_map(CapabilityManifestV1::binding_ids)
        .filter_map(|binding_id| snapshot.binding(binding_id))
        .find(|binding| binding.surface() == surface && binding.operation() == &operation_name)
    else {
        return false;
    };
    let negotiated: BTreeSet<FeatureId> = binding.required_features().iter().cloned().collect();
    snapshot.profiles().any(|profile| {
        snapshot
            .resolve_binding(
                profile.profile_id(),
                surface,
                &operation_name,
                NEGOTIATED_PROTOCOL_REVISION,
                &negotiated,
            )
            .is_some()
    })
}

#[test]
fn every_catalog_binding_is_mounted_on_its_declared_surface() {
    let snapshot = build_application_catalog_snapshot().expect("application catalog snapshot");
    let mut declared: BTreeMap<(BindingSurface, String), (String, String)> = BTreeMap::new();
    for capability in snapshot.capabilities() {
        for binding_id in capability.binding_ids() {
            let binding = snapshot.binding(binding_id).unwrap_or_else(|| {
                panic!(
                    "capability {} names binding {} that the snapshot does not hold, so the \
                     sweep below would grade a silently truncated set",
                    capability.capability_id().as_str(),
                    binding_id.as_str()
                )
            });
            declared.insert(
                (binding.surface(), binding.operation().as_str().to_owned()),
                (
                    binding_id.as_str().to_owned(),
                    capability.capability_id().as_str().to_owned(),
                ),
            );
        }
    }
    assert!(
        !declared.is_empty(),
        "the runtime catalog snapshot declared no surface bindings, so this \
         contract would pass vacuously"
    );

    let fixture = MountFixture::start();
    let agent = common::http_agent_with_timeout(Duration::from_secs(30));
    assert_external_surface_discriminates(&agent, &fixture);
    let cli_tools = cli_tool_listing(&fixture);
    let mcp_tools = mcp_tool_listing(&fixture);
    let dashboard = DashboardProbe::start(&fixture);
    let mut lsp = LspBridgeProbe::start(&fixture);

    let mut failures = Vec::new();
    let mut graded_by_surface: BTreeMap<BindingSurface, usize> = BTreeMap::new();
    for ((surface, operation), (binding_id, capability_id)) in &declared {
        *graded_by_surface.entry(*surface).or_default() += 1;
        let subject = format!("{}:{operation}", surface_label(*surface));
        let note = format!("{subject} (binding {binding_id}, capability {capability_id})");

        if !binding_resolves(&snapshot, *surface, operation) {
            failures.push(format!(
                "{note}: declared in the catalog snapshot but the production \
                 binding resolver answers nothing for it, so no adapter can \
                 dispatch it"
            ));
            continue;
        }

        let mounted = match surface {
            BindingSurface::Http => {
                match ApplicationSurfaceOperation::from_catalog_name(operation) {
                    Some(http)
                        if is_http_application_operation_exposed(http)
                            .expect("HTTP exposure registry") =>
                    {
                        http_route_is_mounted(
                            &agent,
                            &fixture,
                            &http_application_full_route_path(http),
                        )
                    }
                    // An operation the router deliberately withholds from HTTP is
                    // an absence like any other.
                    Some(_) | None => false,
                }
            }
            // An internal owner operation is served by name for first-party
            // CLI commands and host hooks; being listed is the defect.
            BindingSurface::Mcp | BindingSurface::Cli
                if ApplicationSurfaceOperation::from_surface_name(*surface, operation)
                    .is_some_and(ApplicationSurfaceOperation::is_internal) =>
            {
                let listed = match surface {
                    BindingSurface::Mcp => mcp_tools.contains(&format!("tracedecay_{operation}")),
                    _ => cli_tools.contains(operation.as_str()),
                };
                if listed {
                    failures.push(format!(
                        "{note}: an internal owner operation is advertised in the {surface:?} listing"
                    ));
                }
                continue;
            }
            BindingSurface::Mcp => mcp_tools.contains(&format!("tracedecay_{operation}")),
            BindingSurface::Cli => cli_tools.contains(operation.as_str()),
            // The LSP and dashboard adapters have no listing endpoint of their
            // own, so they are probed the way a client drives them: a real
            // `lsp bridge` session and the live dashboard server's closed
            // route tables.
            BindingSurface::Lsp => lsp.operation_is_mounted(operation),
            BindingSurface::Dashboard => dashboard.operation_is_mounted(operation),
        };

        if !mounted && sanctioned_citation(&subject).is_none() {
            failures.push(match surface {
                BindingSurface::Http => format!(
                    "{note}: no route registered at the live daemon's HTTP \
                     application endpoint"
                ),
                BindingSurface::Mcp => format!(
                    "{note}: tracedecay_{operation} is absent from the live MCP \
                     server's tools/list answer"
                ),
                BindingSurface::Cli => {
                    format!("{note}: absent from the `tracedecay tool` command listing")
                }
                BindingSurface::Lsp => format!(
                    "{note}: the live lsp bridge answered Method unavailable \
                     (-32601 explicitlyUnavailable) for this catalog-declared \
                     operation"
                ),
                BindingSurface::Dashboard => format!(
                    "{note}: no route in the dashboard server's closed \
                     dispatch table answers this catalog-declared operation"
                ),
            });
        }
    }

    eprintln!(
        "forward sweep graded {} catalog binding(s): {}",
        declared.len(),
        graded_by_surface
            .iter()
            .map(|(surface, count)| format!("{}={count}", surface_label(*surface)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert!(
        failures.is_empty(),
        "{}",
        report(
            &format!(
                "{} of {} catalog-declared bindings are not reachable on the \
                 surface they declare. Either mount them or add the absence to \
                 SANCTIONED_UNMOUNTED with the plan that sanctions it.",
                failures.len(),
                declared.len()
            ),
            &failures,
        )
    );
}

const fn surface_label(surface: BindingSurface) -> &'static str {
    match surface {
        BindingSurface::Cli => "cli",
        BindingSurface::Mcp => "mcp",
        BindingSurface::Http => "http",
        BindingSurface::Lsp => "lsp",
        BindingSurface::Dashboard => "dashboard",
    }
}

#[test]
fn every_declared_operation_is_mounted_or_sanctioned() {
    let snapshot = build_application_catalog_snapshot().expect("application catalog snapshot");
    let mut catalog_http_operations = BTreeSet::new();
    for capability in snapshot.capabilities() {
        for binding_id in capability.binding_ids() {
            if let Some(binding) = snapshot.binding(binding_id)
                && binding.surface() == BindingSurface::Http
            {
                catalog_http_operations.insert(binding.operation().as_str().to_owned());
            }
        }
    }

    let fixture = MountFixture::start();
    let agent = common::http_agent_with_timeout(Duration::from_secs(30));
    assert_external_surface_discriminates(&agent, &fixture);
    let cli_tools = cli_tool_listing(&fixture);
    let mcp_tools = mcp_tool_listing(&fixture);

    let mut failures = Vec::new();
    let mut graded = 0usize;

    // -- Work operations. ---------------------------------------------------
    // `WorkOperation::ALL` documents itself as "every mounted Work operation,
    // in mounted order"; this is what makes that claim testable.
    for operation in WorkOperation::ALL {
        graded += 1;
        let route = operation.application_route_path();
        let subject = format!("work:{}", operation.route_segment());
        if !http_route_is_mounted(&agent, &fixture, route)
            && sanctioned_citation(&subject).is_none()
        {
            failures.push(format!(
                "{subject}: WorkOperation::ALL names it a mounted operation but \
                 the live daemon serves no route at {route}"
            ));
        }
        graded += 1;
        let mcp_subject = format!("mcp:work:{}", operation.route_segment());
        let mcp_tool = format!("tracedecay_work_{}", operation.operation_key());
        if !mcp_tools.contains(&mcp_tool) && sanctioned_citation(&mcp_subject).is_none() {
            failures.push(format!(
                "{mcp_subject}: WorkOperation::ALL names it a mounted operation \
                 but {mcp_tool} is absent from the live MCP server's tools/list \
                 answer"
            ));
        }
    }

    // -- Workflow operations. -----------------------------------------------
    // Graded on both declared external surfaces. Checking only
    // HTTP here is how the entire sixteen-operation family came to be mounted
    // on CLI and HTTP while carrying no MCP tool at all: every row passed, and
    // the absence was invisible because nothing ever asked the question. A
    // closed family that publishes a transport-independent descriptor has to be
    // graded against every transport that descriptor claims, or the sweep only
    // proves the surface it happened to look at.
    for operation in WorkflowOperation::ALL {
        graded += 1;
        let route = operation.application_route_path();
        let subject = format!("workflow:{}", operation.route_segment());
        if !http_route_is_mounted(&agent, &fixture, route)
            && sanctioned_citation(&subject).is_none()
        {
            failures.push(format!(
                "{subject}: declared by WorkflowOperation::ALL but the live \
                 daemon serves no route at {route}"
            ));
        }
        graded += 1;
        let mcp_subject = format!("mcp:workflow:{}", operation.route_segment());
        let mcp_tool = format!("tracedecay_workflow_{}", operation.operation_key());
        if !mcp_tools.contains(&mcp_tool) && sanctioned_citation(&mcp_subject).is_none() {
            failures.push(format!(
                "{mcp_subject}: declared by WorkflowOperation::ALL but \
                 {mcp_tool} is absent from the live MCP server's tools/list \
                 answer"
            ));
        }
    }

    // -- HTTP application operations. ---------------------------------------
    for operation in ApplicationSurfaceOperation::ALL {
        graded += 1;
        let name = operation.as_str();
        let subject = format!("http:{name}");
        if sanctioned_citation(&subject).is_some() {
            continue;
        }
        // An operation the router deliberately withholds (Git preview/apply
        // and native-integration apply are CLI/MCP-only, because apply is an
        // authoritative native mutation with no transport fallback) must not
        // be required to carry an HTTP catalog binding.
        // `is_http_exposed` is the single authority for that decision, so the
        // catalog requirement and the route requirement consult it alike.
        if !is_http_application_operation_exposed(operation).expect("HTTP exposure registry") {
            continue;
        }
        if !catalog_http_operations.contains(name) {
            failures.push(format!(
                "{subject}: the HTTP router declares this operation but no \
                 catalog capability binds it to the HTTP surface, so it can \
                 never be authorized or discovered"
            ));
            continue;
        }
        if !http_route_is_mounted(
            &agent,
            &fixture,
            &http_application_full_route_path(operation),
        ) {
            failures.push(format!(
                "{subject}: catalog-declared and HTTP-exposed, but the live \
                 daemon serves no route at {}",
                http_application_full_route_path(operation)
            ));
        }
    }

    // Callable-code transport parity: CLI and MCP must expose the same
    // application operations as HTTP. A code operation reachable on one
    // transport and not the others is a half-mounted surface, which no
    // single-transport sweep would catch.
    for operation in CALLABLE_CODE_OPERATIONS {
        let name = operation.as_str();
        for (surface, present) in [
            (BindingSurface::Http, catalog_http_operations.contains(name)),
            (
                BindingSurface::Cli,
                cli_tools.contains(operation.name_for_surface(BindingSurface::Cli)),
            ),
            (
                BindingSurface::Mcp,
                mcp_tools.contains(operation.mcp_tool_name()),
            ),
        ] {
            graded += 1;
            let subject = format!("{}:{name}", surface_label(surface));
            if !present && sanctioned_citation(&subject).is_none() {
                failures.push(format!(
                    "{subject}: callable-code operations must be exposed on \
                     CLI, MCP, and HTTP alike, and this one is not published \
                     on {}",
                    surface_label(surface)
                ));
            }
        }
    }

    // -- Activity families. -------------------------------------------------
    // Each family names one dashboard event stream. The exhaustive match is
    // the maintenance device: a new family fails to compile until it is
    // classified as produced or sanctioned, so it cannot be added and
    // silently never emitted. A family classified as produced must not also
    // appear in the sanctioned table, that would hide a real regression
    // behind a stale exemption.
    for family in ActivityFamilyV1::ALL {
        graded += 1;
        let has_producer = match family {
            ActivityFamilyV1::Hook
            | ActivityFamilyV1::SessionIngest
            | ActivityFamilyV1::CodeIndex
            | ActivityFamilyV1::ToolCall
            | ActivityFamilyV1::Task => true,
        };
        let subject = format!("activity-family:{}", family.stream_name());
        match (has_producer, sanctioned_citation(&subject)) {
            (true, Some(_)) => failures.push(format!(
                "{subject}: classified as produced here but still listed in \
                 SANCTIONED_UNMOUNTED; drop the stale exemption"
            )),
            (false, None) => failures.push(format!(
                "{subject}: no production site publishes this family, so the \
                 stream is never emitted and the dashboard renders nothing for \
                 it. Land the producer or add the absence to \
                 SANCTIONED_UNMOUNTED with the plan that sanctions it."
            )),
            (true, None) | (false, Some(_)) => {}
        }
    }

    // -- Work product read model. -------------------------------------------
    // The Work product graph has a durable authority but its projection bundle
    // reaches no client until it is either routed or carried in the dashboard
    // wire contract. Rendering the real contract is the product check: no
    // source scan, just the schema the dashboard actually publishes.
    graded += 1;
    let dashboard_contract =
        tracedecay_dashboard_api::contract_schema::render_dashboard_contract_schema()
            .expect("dashboard wire contract schema");
    let subject = "dashboard-wire-contract:WorkProductProjectionBundleV1";
    let in_contract = dashboard_contract.contains("WorkProductProjectionBundleV1");
    match (in_contract, sanctioned_citation(subject)) {
        (true, Some(_)) => failures.push(format!(
            "{subject}: now carried by the dashboard wire contract but still \
             listed in SANCTIONED_UNMOUNTED; drop the stale exemption"
        )),
        (false, None) => failures.push(format!(
            "{subject}: the Work product projection bundle reaches no client, \
             it is absent from the dashboard wire contract and has no \
             application route"
        )),
        (true, None) | (false, Some(_)) => {}
    }

    eprintln!("reverse sweep graded {graded} independently declared surface(s)");
    assert!(
        failures.is_empty(),
        "{}",
        report(
            &format!(
                "{} of {graded} independently declared surfaces are unmounted. \
                 Either mount them or add the absence to SANCTIONED_UNMOUNTED \
                 with the plan that sanctions it.",
                failures.len(),
            ),
            &failures,
        )
    );
}
