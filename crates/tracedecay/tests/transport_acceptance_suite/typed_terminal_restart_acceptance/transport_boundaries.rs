//! The HTTP, MCP-host, Rust SDK, and TypeScript SDK typed-terminal journey.
//!
//! The CLI leg lives in this target's root module. It induces both terminals
//! through real production mechanisms, a fact that commits durably and then
//! outlives its caller's request deadline at the daemon's own commit boundary,
//! and a project store whose relational shape this binary refuses, and proves
//! each survives a physical daemon kill and respawn.
//!
//! This module drives those same two genuinely-induced terminals through the
//! remaining mounted transports, against the same *physically* spawned
//! `tracedecay daemon run` process:
//!
//! - **HTTP**: the daemon's own bound HTTP application mount. `bootstrap.rs`
//!   binds `DaemonHttpApplicationService` on loopback and publishes its
//!   endpoint and bearer token into `<profile>/daemon-authority.json`; requests
//!   go over real TCP to `/projects/{project_id}/application/...`, which is the
//!   production `http_application_router`. The caller's request deadline rides
//!   in the `x-tracedecay-deadline-micros` header that
//!   `application_http_context`
//!   (`tracedecay-daemon-service/src/application_surface.rs`) reads.
//!
//! - **MCP**: a real `tracedecay serve` stdio host, spawned per call, speaking
//!   JSON-RPC `initialize` + `tools/call` exactly as an MCP client does. The
//!   caller's deadline rides in the standard `_meta` object under
//!   `tracedecay/deadline-micros` (`src/mcp/tool_call_deadline.rs`).
//!
//! - **Rust SDK**: `tracedecay_sdk::client::Client` in `ConnectionMode::local`,
//!   dialing the same published endpoint with the same token, executing the
//!   generated typed operations. Its `OperationRequestOptions::deadline_micros`
//!   is the SDK's own name for the same header.
//!
//! - **TypeScript SDK**: a Node process imports the built generated client,
//!   calls its mounted operation, and verifies the concrete typed error class.
//!
//! Every leg asserts the terminal's kind, its legal actions, and, for
//! `PartialEffect`, the committed receipt, then the daemon process is replaced
//! and the same contract is re-asserted against the new process over the same
//! on-disk state.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use tracedecay_sdk::client::{Client, ClientError, ConnectionMode, OperationRequestOptions};
use tracedecay_sdk::operations::{ApplicationFactStoreAdd, ApplicationStorageStatus};
use tracedecay_session_memory::memory::hygiene::detect_secret_like;

use crate::common::{
    TestChildProcess, http_agent_with_timeout, output_with_timeout, tracedecay_command_with_home,
};

/// Route of the fact-add effect on the application mount, as the generated SDK
/// operation descriptor names it.
const FACT_STORE_ADD_ROUTE: &str = "/application/retained/fact_store_add";
/// Route of the storage-status read on the application mount.
const STORAGE_STATUS_ROUTE: &str = "/application/primitives/storage_status";
/// Header the HTTP application middleware reads as the caller's request
/// deadline, in absolute UTC microseconds.
const HTTP_DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";
/// `_meta` key naming the caller's request deadline on `tools/call`.
const MCP_DEADLINE_META_KEY: &str = "tracedecay/deadline-micros";
/// How long to wait for the daemon to publish its authority record.
const AUTHORITY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on one `tracedecay serve` stdio exchange. It must outlive a parked
/// commit, whose whole point is that it settles after its own deadline.
const SERVE_TIMEOUT: Duration = Duration::from_secs(90);
/// Bound on one HTTP or SDK call. Same reasoning as `SERVE_TIMEOUT`: the
/// response the journey wants is produced *because* the request deadline
/// elapsed, so the transport read must outlive it.
const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(90);

/// The daemon's published HTTP application mount.
struct HttpMount {
    base_url: String,
    origin: String,
    token: String,
}

/// Reads the physically spawned daemon's authority record, waiting until it
/// has published both a bearer token and an HTTP application endpoint.
fn http_mount(home: &Path) -> HttpMount {
    let authority = crate::common::daemon_authority_path(&home.join(".tracedecay"));
    let deadline = Instant::now() + AUTHORITY_TIMEOUT;
    loop {
        if let Ok(bytes) = std::fs::read(&authority)
            && let Ok(record) = serde_json::from_slice::<Value>(&bytes)
            && let (Some(token), Some(endpoint)) = (
                record["auth_token"].as_str(),
                record["http_application_endpoint"].as_str(),
            )
            && !token.is_empty()
            && !endpoint.is_empty()
        {
            let base_url = format!("http://{endpoint}");
            return HttpMount {
                origin: base_url.clone(),
                base_url,
                token: token.to_owned(),
            };
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never published an HTTP application endpoint at {}",
            authority.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Posts to the daemon's HTTP mount, repeating while the daemon answers with a
/// pre-admission problem whose own retry directive is `after_delay`.
///
/// The authority record can be published before a restarted daemon has opened
/// the project, and the reply for that window is a typed, retryable
/// `unavailable`, not a verdict on the project. The first observation a
/// journey asserts on is the first one the daemon *admitted*, which is what a
/// production client that honours the directive sees.
fn post_application_once_admitted(
    mount: &HttpMount,
    project_id: &str,
    route: &str,
    body: &Value,
) -> (u16, Value) {
    let deadline = Instant::now() + AUTHORITY_TIMEOUT;
    loop {
        let (status, payload) = post_application(mount, project_id, route, body, None);
        let problem = [&payload, &payload["value"], &payload["data"]]
            .into_iter()
            .map(|candidate| &candidate["problem"])
            .find(|problem| problem.is_object())
            .filter(|problem| problem["terminality"] == "pre_admission")
            .filter(|problem| problem["retry"] == "after_delay");
        match problem {
            Some(problem) if Instant::now() < deadline => {
                let millis = problem["retry_after_millis"].as_u64().unwrap_or(250);
                std::thread::sleep(Duration::from_millis(millis));
            }
            _ => return (status, payload),
        }
    }
}

/// Opens the exact project through the same daemon-owned route as a production
/// CLI client and returns the identity the daemon admitted. HTTP cannot infer
/// this identity locally: its route accepts only the daemon's public ID.
fn admitted_project_id(home: &Path, project: &Path) -> String {
    let project_arg = project.to_string_lossy().into_owned();
    let deadline = Instant::now() + AUTHORITY_TIMEOUT;
    let output = loop {
        let output = tracedecay_command_with_home(home)
            .current_dir(project)
            .args([
                "tool",
                "--project",
                project_arg.as_str(),
                "storage_status",
                "--args",
                r#"{"include_details":false}"#,
                "--json",
            ])
            .stdin(Stdio::null())
            .output()
            .expect("read daemon-admitted project identity");
        // Right after `init` the daemon can still be mounting the project's
        // query authority; it says so with a pre-admission problem whose own
        // retry directive is `after_delay`. Honour that directive, as a
        // production client would, instead of treating it as a verdict.
        let problem = serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .map(|printed| printed["structuredContent"]["problem"].clone())
            .filter(|problem| problem["terminality"] == "pre_admission")
            .filter(|problem| problem["retry"] == "after_delay");
        match problem {
            Some(problem) if !output.status.success() && Instant::now() < deadline => {
                let millis = problem["retry_after_millis"].as_u64().unwrap_or(250);
                std::thread::sleep(Duration::from_millis(millis));
            }
            _ => break output,
        }
    };
    assert!(
        output.status.success(),
        "storage_status failed while admitting the fixture project\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = crate::common::tool_json_structured_content(&output.stdout);
    assert!(
        envelope["problem"].is_null() && !envelope["outcome"].is_null(),
        "storage_status must complete before the typed-terminal journey starts: {envelope}"
    );
    assert_eq!(
        envelope["outcome"]["outcome"], "evidence",
        "storage_status must return retained storage evidence: {envelope}"
    );
    assert_eq!(
        envelope["outcome"]["value"]["payload"]["status"], "ok",
        "storage_status must report an admitted healthy store: {envelope}"
    );
    let project_id = envelope["scope"]["project_id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("storage status omitted the daemon-admitted project identity: {envelope}")
        })
        .to_owned();
    assert_eq!(
        envelope["outcome"]["value"]["payload"]["project_id"], project_id,
        "storage evidence must belong to the daemon-admitted project: {envelope}"
    );
    project_id
}

fn now_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after the unix epoch")
            .as_micros(),
    )
    .expect("current time fits in i64 microseconds")
}

/// Absolute deadline this journey gives a parked request, in UTC micros.
fn parked_request_deadline_micros() -> i64 {
    now_micros()
        + i64::try_from(super::PARTIAL_EFFECT_DEADLINE.as_micros())
            .expect("the parked request budget fits in i64 microseconds")
}

/// POSTs one application operation over real TCP to the daemon's own mount.
fn post_application(
    mount: &HttpMount,
    project_id: &str,
    route: &str,
    body: &Value,
    deadline_micros: Option<i64>,
) -> (u16, Value) {
    let route = route
        .strip_prefix("/application")
        .expect("an application route begins with /application");
    let url = format!(
        "{}/projects/{project_id}/application{route}",
        mount.base_url
    );
    let agent = http_agent_with_timeout(TRANSPORT_TIMEOUT);
    let mut request = agent
        .post(&url)
        .header("authorization", format!("Bearer {}", mount.token))
        .header("origin", mount.origin.as_str())
        .header("content-type", "application/json");
    if let Some(deadline) = deadline_micros {
        request = request.header(HTTP_DEADLINE_HEADER, deadline.to_string());
    }
    let response = request
        .send_json(body)
        .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
    crate::common::response_to_json(response)
}

fn sdk_client(mount: &HttpMount, project_id: &str) -> Client {
    Client::builder(ConnectionMode::local(
        mount.base_url.clone(),
        project_id.to_owned(),
        mount.token.clone(),
    ))
    .origin(mount.origin.clone())
    .timeout(TRANSPORT_TIMEOUT)
    .build()
    .expect("build a Rust SDK client against the daemon's published mount")
}

/// Drives one `tools/call` through a real `tracedecay serve` MCP host.
///
/// The host is spawned per call and exits when stdin closes, which is the same
/// one-shot shape the repository's other stdio MCP journeys use.
fn mcp_tool_call(
    home: &Path,
    project: &Path,
    tool: &str,
    arguments: &Value,
    deadline_micros: Option<i64>,
) -> Value {
    let mut command = tracedecay_command_with_home(home);
    command
        .arg("serve")
        .arg("--path")
        .arg(project)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = TestChildProcess::new(
        command
            .spawn()
            .expect("spawn the tracedecay MCP stdio host"),
    );

    let mut params = json!({ "name": tool, "arguments": arguments });
    if let Some(deadline) = deadline_micros {
        params["_meta"] = json!({ MCP_DEADLINE_META_KEY: deadline });
    }
    {
        let stdin = child.stdin_mut().expect("MCP host stdin is piped");
        let _ = writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "typed-terminal-transport-boundaries",
                        "version": "0.0.0"
                    }
                }
            })
        );
        let _ = writeln!(
            stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": params })
        );
    }

    let output = child
        .wait_with_output(SERVE_TIMEOUT)
        .expect("the MCP stdio host exits after stdin closes");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message.get("id") == Some(&json!(2)))
        .unwrap_or_else(|| {
            panic!(
                "the MCP host returned no response for '{tool}'\nstdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
}

/// The typed application payload an MCP tool result carries.
///
/// A tool result's single text block is the rendered envelope; asking for JSON
/// makes that block the envelope itself. A tool that answered with a JSON-RPC
/// error instead carries the envelope in the error's data.
fn mcp_payload(response: &Value) -> Value {
    if let Some(text) = response["result"]["content"][0]["text"].as_str()
        && let Ok(parsed) = serde_json::from_str::<Value>(text)
    {
        return parsed;
    }
    if response.get("error").is_some() {
        return response["error"].clone();
    }
    response["result"].clone()
}

/// Normalizes any transport's payload to `{ "problem": ... }`.
///
/// The transports wrap the canonical problem envelope differently, a bare
/// envelope, a `value` body, an `outcome.value`, MCP `structuredContent`, but
/// the envelope itself is the contract under test, so the journey asserts
/// against it wherever the transport parked it rather than hard-coding one
/// wrapper.
fn problem_envelope(payload: &Value, context: &str) -> Value {
    for candidate in [
        payload.clone(),
        payload["value"].clone(),
        payload["data"].clone(),
        payload["outcome"]["value"].clone(),
        payload["structuredContent"].clone(),
    ] {
        if candidate["problem"].is_object() {
            return json!({ "problem": candidate["problem"].clone() });
        }
    }
    panic!("{context}: no typed problem envelope in the payload: {payload}")
}

/// True when the daemon has not published the project open yet.
///
/// The open wait on a connection is 500 ms. Past that, the surface answers
/// `unavailable` / `after_delay` and leaves the open running. The CLI rides
/// that refusal out; a raw HTTP or SDK call does not. A restart's first packet
/// can therefore be warming even when the store's recorded terminal is
/// `reset_required`.
fn retryable_pre_admission_unavailable(payload: &Value) -> bool {
    let problem = &payload["problem"];
    problem["kind"] == "unavailable"
        && problem["retry"] == "after_delay"
        && problem["terminality"] == "pre_admission"
}

/// Polls until the open publishes a non-retryable problem, then returns it.
///
/// Bounded by the same 15 s a CLI tool call gives a cold open. A refusal that
/// stays retryable past that bound is a real failure, not a slow open.
fn await_settled_problem(context: &str, mut fetch: impl FnMut() -> Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let payload = fetch();
        if !retryable_pre_admission_unavailable(&payload) {
            return payload;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: project open stayed retryable unavailable past the open grace: {payload}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn await_sdk_reset_problem(
    context: &str,
    client: &Client,
    request: &<ApplicationStorageStatus as tracedecay_sdk::operations::TypedOperation>::Request,
) -> (String, Value) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let error = client
            .execute::<ApplicationStorageStatus>(request)
            .expect_err("a refused store must not read as a healthy status");
        let (kind, envelope) = sdk_problem(error, context);
        let payload = problem_envelope(&envelope, context);
        if !retryable_pre_admission_unavailable(&payload) {
            return (kind, envelope);
        }
        assert!(
            Instant::now() < deadline,
            "{context}: project open stayed retryable unavailable past the open grace: {payload}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Arms the daemon's one-shot fact-commit barrier, runs `request` on its own
/// thread, holds the committed effect there until the request's own deadline
/// has certainly expired, then releases it and returns what `request` produced.
///
/// The hold is measured from *arrival*, not from spawn, because the request
/// starts its deadline clock itself: arrival is strictly after that clock
/// started, so holding a full budget plus a margin beyond arrival always
/// outlives it.
fn park_at_commit_barrier<T, F>(barrier_dir: &Path, content: &str, request: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // The barrier is one-shot per directory: clear a previous park's markers
    // so the same daemon process can serve more than one parked request.
    for marker in ["armed", "claimed", "arrived", "release", "expect_content"] {
        let _ = std::fs::remove_file(barrier_dir.join(marker));
    }
    std::fs::write(barrier_dir.join("expect_content"), content.as_bytes())
        .expect("pin the fact commit barrier to the parked content");
    std::fs::write(barrier_dir.join("armed"), b"armed\n").expect("arm the fact commit barrier");

    let started = Instant::now();
    let handle = std::thread::spawn(request);

    let arrived = barrier_dir.join("arrived");
    while !matches!(arrived.try_exists(), Ok(true)) {
        assert!(
            !handle.is_finished(),
            "the request settled without ever reaching the durable commit boundary"
        );
        assert!(
            started.elapsed() < super::BARRIER_ARRIVAL_TIMEOUT,
            "the request never reached the durable commit boundary within {:?}",
            super::BARRIER_ARRIVAL_TIMEOUT
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let release_at = Instant::now() + super::PARTIAL_EFFECT_DEADLINE + Duration::from_secs(1);
    while Instant::now() < release_at {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(barrier_dir.join("release"), b"release\n")
        .expect("release the fact commit barrier");

    handle.join().expect("join the parked request")
}

fn fact_add_body(content: &str) -> Value {
    json!({ "content": content, "category": "general" })
}

/// Asserts the SDK surfaced a typed terminal rather than a success or a
/// transport failure, and returns its canonical envelope.
fn sdk_problem(error: ClientError, context: &str) -> (String, Value) {
    match error {
        ClientError::Problem(problem) => (problem.kind.clone(), problem.envelope.clone()),
        other => panic!("{context}: the SDK reported {other} instead of a typed terminal"),
    }
}

fn build_typescript_sdk() -> tempfile::TempDir {
    let sdk_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdks/typescript");
    let output_dir = tempfile::TempDir::new().expect("isolated TypeScript SDK output");
    // Nextest runs each journey in a separate process. Both generated output
    // and Rspack's persistent cache must belong to that journey, and building
    // must finish before any request deadline starts.
    let config_url = url::Url::from_file_path(sdk_root.join("rslib.config.ts"))
        .expect("canonical SDK config URL");
    let config_path = output_dir.path().join("rslib.config.mjs");
    std::fs::write(
        &config_path,
        format!(
            "import config from {};\nexport default {{ ...config, performance: {{ ...config.performance, buildCache: {{ cacheDirectory: {} }} }} }};\n",
            serde_json::to_string(config_url.as_str()).expect("SDK config module specifier"),
            serde_json::to_string(&output_dir.path().join("cache")).expect("SDK cache directory"),
        ),
    )
    .expect("configure isolated SDK build cache");
    std::fs::write(
        output_dir.path().join("package.json"),
        r#"{"type":"module"}"#,
    )
    .expect("declare the generated SDK module format");
    #[cfg(windows)]
    let mut build = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "pnpm"]);
        command
    };
    #[cfg(not(windows))]
    let mut build = std::process::Command::new("pnpm");
    build
        .args(["run", "build", "--config"])
        .arg(config_path)
        .arg("--dist-path")
        .arg(output_dir.path().join("dist"))
        .current_dir(&sdk_root);
    let output = output_with_timeout(build, TRANSPORT_TIMEOUT);
    assert!(
        output.status.success(),
        "TypeScript SDK build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output_dir
}

fn typescript_problem(
    sdk_output: &Path,
    mount: &HttpMount,
    project_id: &str,
    operation: &str,
    request: &Value,
    deadline_micros: Option<i64>,
) -> Value {
    typescript_call(
        sdk_output,
        mount,
        project_id,
        operation,
        request,
        deadline_micros,
        false,
    )
}

fn typescript_call(
    sdk_output: &Path,
    mount: &HttpMount,
    project_id: &str,
    operation: &str,
    request: &Value,
    deadline_micros: Option<i64>,
    allow_success: bool,
) -> Value {
    let sdk_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdks/typescript");
    let mut command = std::process::Command::new("node");
    command
        .arg("test/typed-terminal-live.mjs")
        .arg(sdk_output.join("dist/index.js"))
        .current_dir(&sdk_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        TestChildProcess::new(command.spawn().expect("spawn the TypeScript SDK journey"));
    writeln!(child.stdin_mut().expect("SDK input"), "{}", json!({
        "connection": { "baseUrl": mount.base_url, "origin": mount.origin, "token": mount.token, "projectId": project_id },
        "operation": operation, "request": request, "deadlineMicros": deadline_micros,
        "allowSuccess": allow_success,
    })).expect("write the SDK request");
    let output = child
        .wait_with_output(TRANSPORT_TIMEOUT)
        .expect("TypeScript SDK call finishes");
    assert!(
        output.status.success(),
        "TypeScript SDK call failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("TypeScript SDK problem envelope")
}

#[test]
fn partial_effect_survives_http_mcp_and_both_sdks_across_restart() {
    let sdk_output = build_typescript_sdk();
    // Each marker stays short enough that production memory hygiene stores
    // the fact instead of refusing it as a high-entropy secret-like token.
    const HTTP_MARKER: &str = "boundaries-partial-http-4a71c8";
    const MCP_MARKER: &str = "boundaries-partial-mcp-4a71c8";
    const SDK_MARKER: &str = "boundaries-partial-sdk-4a71c8";
    const TS_MARKER: &str = "boundaries-partial-typescript-4a71c8";
    const POST_RESTART_MARKER: &str = "boundaries-partial-restart-4a71c8";
    /// Query that must retrieve every marker above after the restart.
    const MARKER_QUERY: &str = "boundaries-partial";

    // A hygiene refusal must never masquerade as the partial-effect terminal
    // this suite induces.
    for marker in [
        HTTP_MARKER,
        MCP_MARKER,
        SDK_MARKER,
        TS_MARKER,
        POST_RESTART_MARKER,
    ] {
        assert!(
            detect_secret_like(marker).is_none(),
            "marker {marker:?} would be refused as secret-like before the commit boundary"
        );
    }

    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = crate::common::canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("partial effect project");
    let project_path = crate::common::canonical_existing_path(project.path());
    let barrier = tempfile::TempDir::new().expect("fact commit barrier");
    let barrier_path = crate::common::canonical_existing_path(barrier.path());

    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    super::initialize_project(&home_path, &project_path, "partial-effect-boundaries");
    let identity = admitted_project_id(&home_path, &project_path);
    let mount = http_mount(&home_path);

    // HTTP: the daemon's own bound application mount, over real TCP, with the
    // caller's deadline in the header the middleware reads.
    let http_mount_for_request = HttpMount {
        base_url: mount.base_url.clone(),
        origin: mount.origin.clone(),
        token: mount.token.clone(),
    };
    let http_identity = identity.clone();
    let (http_status, http_body) = park_at_commit_barrier(&barrier_path, HTTP_MARKER, move || {
        post_application(
            &http_mount_for_request,
            &http_identity,
            FACT_STORE_ADD_ROUTE,
            &fact_add_body(HTTP_MARKER),
            Some(parked_request_deadline_micros()),
        )
    });
    super::assert_partial_effect_committed_receipt(
        &problem_envelope(&http_body, "HTTP partial effect"),
        "HTTP mount, pre-restart",
    );
    assert!(
        (400..600).contains(&http_status),
        "a typed HTTP terminal must not be reported as success: status {http_status}, body {http_body}"
    );

    // MCP: a real `tracedecay serve` stdio host carrying the same deadline in
    // the `_meta` object on `tools/call`.
    let mcp_home = home_path.clone();
    let mcp_project = project_path.clone();
    let mcp_response = park_at_commit_barrier(&barrier_path, MCP_MARKER, move || {
        mcp_tool_call(
            &mcp_home,
            &mcp_project,
            "tracedecay_fact_store_add",
            &fact_add_body(MCP_MARKER),
            Some(parked_request_deadline_micros()),
        )
    });
    super::assert_partial_effect_committed_receipt(
        &problem_envelope(&mcp_payload(&mcp_response), "MCP partial effect"),
        "MCP stdio host, pre-restart",
    );

    // Rust SDK: the generated typed operation, same endpoint, same token, with
    // the SDK's own name for the caller's deadline.
    let sdk_mount = HttpMount {
        base_url: mount.base_url.clone(),
        origin: mount.origin.clone(),
        token: mount.token.clone(),
    };
    let sdk_identity = identity.clone();
    let sdk_error = park_at_commit_barrier(&barrier_path, SDK_MARKER, move || {
        let client = sdk_client(&sdk_mount, &sdk_identity);
        let request =
            serde_json::from_value(fact_add_body(SDK_MARKER)).expect("canonical fact-add request");
        match client.execute_with_options::<ApplicationFactStoreAdd>(
            &request,
            OperationRequestOptions {
                deadline_micros: Some(parked_request_deadline_micros()),
                request_id: None,
            },
        ) {
            Ok(response) => panic!(
                "a parked fact add must not settle as a plain success: {}",
                response.envelope
            ),
            Err(error) => error,
        }
    });
    let (sdk_kind, sdk_envelope) = sdk_problem(sdk_error, "Rust SDK partial effect");
    assert_eq!(
        sdk_kind, "partial_effect",
        "the Rust SDK must classify the terminal as a partial effect: {sdk_envelope}"
    );
    super::assert_partial_effect_committed_receipt(
        &problem_envelope(&sdk_envelope, "Rust SDK partial effect"),
        "Rust SDK, pre-restart",
    );

    let ts_mount = HttpMount {
        base_url: mount.base_url.clone(),
        origin: mount.origin.clone(),
        token: mount.token.clone(),
    };
    let ts_identity = identity.clone();
    let ts_envelope = park_at_commit_barrier(&barrier_path, TS_MARKER, move || {
        typescript_problem(
            sdk_output.path(),
            &ts_mount,
            &ts_identity,
            "fact_store_add",
            &fact_add_body(TS_MARKER),
            Some(parked_request_deadline_micros()),
        )
    });
    let ts_problem = problem_envelope(&ts_envelope, "TypeScript SDK partial effect");
    assert_eq!(ts_problem["problem"]["kind"], "partial_effect");
    super::assert_partial_effect_committed_receipt(&ts_problem, "TypeScript SDK, pre-restart");

    // Physically replace the serving process. Nothing on disk changes.
    let first_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the first physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        first_pid,
        "restart reused the physical daemon process"
    );
    let mount = http_mount(&home_path);
    assert_eq!(
        admitted_project_id(&home_path, &project_path),
        identity,
        "the post-restart daemon must re-admit the same physical project identity"
    );

    // Every committed part of every partial effect is still durably present:
    // the committed half of each committed_receipt outlived the process that
    // reported it.
    let (search_status, search_body) = post_application(
        &mount,
        &identity,
        "/application/retained/fact_store_search",
        &json!({ "query": MARKER_QUERY }),
        None,
    );
    assert_eq!(
        search_status, 200,
        "the post-restart fact search must itself succeed: {search_body}"
    );
    let rendered = search_body.to_string();
    for marker in [HTTP_MARKER, MCP_MARKER, SDK_MARKER, TS_MARKER] {
        assert!(
            rendered.contains(marker),
            "the fact committed by the {marker} partial effect must survive the physical restart: {search_body}"
        );
    }

    // The new process reports the identical terminal contract for a freshly
    // induced partial effect: a restart neither repairs nor reclassifies it.
    let restart_mount = HttpMount {
        base_url: mount.base_url.clone(),
        origin: mount.origin.clone(),
        token: mount.token.clone(),
    };
    let restart_identity = identity.clone();
    let (_, restart_body) = park_at_commit_barrier(&barrier_path, POST_RESTART_MARKER, move || {
        post_application(
            &restart_mount,
            &restart_identity,
            FACT_STORE_ADD_ROUTE,
            &fact_add_body(POST_RESTART_MARKER),
            Some(parked_request_deadline_micros()),
        )
    });
    super::assert_partial_effect_committed_receipt(
        &problem_envelope(&restart_body, "HTTP partial effect after restart"),
        "HTTP mount, post-restart",
    );

    let _ = daemon.kill_and_wait();
}

#[test]
fn reset_required_survives_http_mcp_and_both_sdks_across_restart() {
    let sdk_output = build_typescript_sdk();
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = crate::common::canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("reset required project");
    let project_path = crate::common::canonical_existing_path(project.path());
    let barrier = tempfile::TempDir::new().expect("fact commit barrier");
    let barrier_path = crate::common::canonical_existing_path(barrier.path());

    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    super::initialize_project(&home_path, &project_path, "reset-required-boundaries");
    let identity = admitted_project_id(&home_path, &project_path);

    let first_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the first physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");

    // Tamper the store between processes. `tracedecay init` is daemon-owned,
    // so the first daemon already opened it; the refused shape is what the
    // *next* process observes on its first open. Tampering while that daemon
    // lived would race its writes.
    super::make_store_reset_required(&home_path, &project_path);

    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        first_pid,
        "restart reused the physical daemon process"
    );
    let mount = http_mount(&home_path);

    let storage_status_body = json!({ "include_details": false });

    let (http_status, http_body) = post_application_once_admitted(
        &mount,
        &identity,
        STORAGE_STATUS_ROUTE,
        &storage_status_body,
    );
    super::assert_reset_required(
        &problem_envelope(&http_body, "HTTP reset required"),
        "HTTP mount, first observation",
    );
    assert!(
        (400..600).contains(&http_status),
        "a typed HTTP terminal must not be reported as success: status {http_status}, body {http_body}"
    );

    let mcp_problem = await_settled_problem("MCP stdio host, first observation", || {
        let mcp_response = mcp_tool_call(
            &home_path,
            &project_path,
            "tracedecay_storage_status",
            &storage_status_body,
            None,
        );
        problem_envelope(&mcp_payload(&mcp_response), "MCP reset required")
    });
    super::assert_reset_required(&mcp_problem, "MCP stdio host, first observation");

    let client = sdk_client(&mount, &identity);
    let request =
        serde_json::from_value(storage_status_body.clone()).expect("canonical storage status");
    let (sdk_kind, sdk_envelope) =
        await_sdk_reset_problem("Rust SDK, first observation", &client, &request);
    assert_eq!(
        sdk_kind, "reset_required",
        "the Rust SDK must classify the terminal as reset required: {sdk_envelope}"
    );
    super::assert_reset_required(
        &problem_envelope(&sdk_envelope, "Rust SDK reset required"),
        "Rust SDK, first observation",
    );

    let ts_problem = await_settled_problem("TypeScript SDK, first observation", || {
        problem_envelope(
            &typescript_problem(
                sdk_output.path(),
                &mount,
                &identity,
                "storage_status",
                &storage_status_body,
                None,
            ),
            "TypeScript SDK reset required",
        )
    });
    super::assert_reset_required(&ts_problem, "TypeScript SDK, first observation");

    // Replace the serving process again. The reset-only legal action must be
    // reported identically on every transport: a restart is not a reset.
    let second_pid = daemon.id();
    let stopped = daemon
        .kill_and_wait()
        .expect("force-stop and reap the second physical daemon");
    assert!(!stopped.success(), "forced daemon stop exited cleanly");
    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, &barrier_path);
    assert_ne!(
        daemon.id(),
        second_pid,
        "restart reused the physical daemon process"
    );
    let mount = http_mount(&home_path);

    let http_problem_after = await_settled_problem("HTTP mount, after a physical restart", || {
        let (_, body) = post_application(
            &mount,
            &identity,
            STORAGE_STATUS_ROUTE,
            &storage_status_body,
            None,
        );
        problem_envelope(&body, "HTTP reset required after restart")
    });
    super::assert_reset_required(&http_problem_after, "HTTP mount, after a physical restart");

    let mcp_problem_after =
        await_settled_problem("MCP stdio host, after a physical restart", || {
            let mcp_after = mcp_tool_call(
                &home_path,
                &project_path,
                "tracedecay_storage_status",
                &storage_status_body,
                None,
            );
            problem_envelope(&mcp_payload(&mcp_after), "MCP reset required after restart")
        });
    super::assert_reset_required(
        &mcp_problem_after,
        "MCP stdio host, after a physical restart",
    );

    let client = sdk_client(&mount, &identity);
    let (sdk_kind_after, sdk_envelope_after) =
        await_sdk_reset_problem("Rust SDK reset required after restart", &client, &request);
    assert_eq!(
        sdk_kind_after, "reset_required",
        "the Rust SDK must keep classifying the terminal as reset required: {sdk_envelope_after}"
    );
    super::assert_reset_required(
        &problem_envelope(&sdk_envelope_after, "Rust SDK reset required after restart"),
        "Rust SDK, after a physical restart",
    );

    let ts_problem_after =
        await_settled_problem("TypeScript SDK, after a physical restart", || {
            problem_envelope(
                &typescript_problem(
                    sdk_output.path(),
                    &mount,
                    &identity,
                    "storage_status",
                    &storage_status_body,
                    None,
                ),
                "TypeScript SDK reset required after restart",
            )
        });
    super::assert_reset_required(
        &ts_problem_after,
        "TypeScript SDK, after a physical restart",
    );

    let _ = daemon.kill_and_wait();
}

/// The retained-surfaces lane's missing TypeScript leg: durable memory,
/// refresh handles, and exact native session retrieval across a new daemon.
#[test]
fn typescript_retained_surfaces_keep_identity_across_physical_restart() {
    let sdk_output = build_typescript_sdk();
    const SESSION: &str = "82261fe4-4fb3-4936-8fd7-ed59c1e20aa7";
    const MESSAGE: &str = "Native Codex session keeps the bronze restart proof";
    const FACT: &str = "typescript retained bronze restart fact";
    let home = tempfile::TempDir::new().expect("isolated home");
    let home_path = crate::common::canonical_existing_path(home.path());
    let project = tempfile::TempDir::new().expect("retained project");
    let project_path = crate::common::canonical_existing_path(project.path());
    let barrier = tempfile::TempDir::new().expect("unused commit barrier");
    let source = home_path.join(".codex/sessions/2026/10/01");
    std::fs::create_dir_all(&source).expect("native transcript directory");
    std::fs::write(
        source.join(format!("rollout-2026-10-01T12-00-00-{SESSION}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta", "timestamp": "2026-10-01T12:00:00Z",
                "payload": { "id": SESSION, "cwd": project_path, "model": "gpt-5.5" }
            }),
            json!({
                "type": "event_msg", "timestamp": "2026-10-01T12:00:01Z",
                "payload": { "type": "user_message", "message": MESSAGE }
            })
        ),
    )
    .expect("seed native host transcript");
    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, barrier.path());
    super::initialize_project(&home_path, &project_path, "retained-typescript");
    let identity = admitted_project_id(&home_path, &project_path);
    let mount = http_mount(&home_path);
    let added = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "fact_store_add",
        &fact_add_body(FACT),
        None,
        true,
    );
    assert_eq!(added["kind"], "success", "{added}");
    let fact_id =
        added["value"]["outcome"]["value"]["payload"]["result"]["commit"]["fact_id"].clone();
    assert!(fact_id.is_string(), "add omitted fact identity: {added}");

    let ingested = mcp_tool_call(
        &home_path,
        &project_path,
        "tracedecay_hook_runtime",
        &json!({ "action": "ingest_transcript", "provider": "codex",
            "user_scope": false, "session_id": SESSION, "format": "json" }),
        None,
    );
    assert!(ingested.get("error").is_none(), "{ingested}");
    assert_ne!(ingested["result"]["isError"], true, "{ingested}");
    let load = json!({ "provider": "codex", "session_id": SESSION,
        "limit": 10, "content_limit": 4096 });
    let deadline = Instant::now() + TRANSPORT_TIMEOUT;
    loop {
        let result = typescript_call(
            sdk_output.path(),
            &mount,
            &identity,
            "lcm_load_session",
            &load,
            None,
            true,
        );
        if result["kind"] == "success" && result.to_string().contains(MESSAGE) {
            break;
        }
        assert_ne!(result["problem"]["kind"], "invalid_request", "{result}");
        assert!(
            Instant::now() < deadline,
            "native session never reached LCM: {result}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    let mut refresh = json!({
        "scope": { "kind": "project" },
        "session": { "id": SESSION }, "source": { "scope": "codex" },
        "target": { "temporal_mode": { "kind": "current" }, "grain": "logical_message",
            "frontier": { "observed_through": 0, "committed_through": 0 } }
    });
    let stale = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "session_refresh_begin",
        &refresh,
        None,
        true,
    );
    assert_eq!(stale["problem"]["kind"], "stale", "{stale}");
    assert_eq!(
        stale["problem"]["legal_actions"],
        json!(["refresh"]),
        "{stale}"
    );
    let frontier = stale["problem"]["detail"]["active"]
        .as_u64()
        .expect("stale refusal identifies the admitted frontier");
    assert!(frontier > 0, "{stale}");
    refresh["target"]["frontier"] = json!({
        "observed_through": frontier, "committed_through": frontier
    });
    let begun = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "session_refresh_begin",
        &refresh,
        None,
        true,
    );
    assert_eq!(begun["kind"], "success", "{begun}");
    let begin_payload = &begun["value"]["outcome"]["value"]["payload"];
    let handle = begin_payload["handle"]
        .as_str()
        .expect("refresh handle")
        .to_owned();
    assert_eq!(begin_payload["scope"], "project", "{begun}");
    refresh["handle"] = json!(handle);
    let status = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "session_refresh_status",
        &refresh,
        None,
        true,
    );
    assert_eq!(status["kind"], "success", "{status}");
    let cancelled = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "session_refresh_cancel",
        &refresh,
        None,
        true,
    );
    // A completed refresh keeps its receipt; cancel must never invent a new
    // operation or turn a completed refresh back into an active one.
    assert_eq!(cancelled["kind"], "success", "{cancelled}");
    assert_eq!(
        cancelled["value"]["outcome"]["value"]["payload"]["operation_id"],
        begin_payload["operation_id"],
        "{cancelled}"
    );

    let first_pid = daemon.id();
    daemon.kill_and_wait().expect("reap first daemon");
    let mut daemon = super::spawn_daemon_with_commit_barrier(&home_path, barrier.path());
    assert_ne!(daemon.id(), first_pid);
    let mount = http_mount(&home_path);
    assert_eq!(admitted_project_id(&home_path, &project_path), identity);
    let search = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "fact_store_search",
        &json!({ "query": FACT }),
        None,
        true,
    );
    assert_eq!(search["kind"], "success", "{search}");
    assert!(
        search
            .to_string()
            .contains(fact_id.as_str().expect("fact identity")),
        "{search}"
    );
    let loaded = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "lcm_load_session",
        &load,
        None,
        true,
    );
    assert_eq!(loaded["kind"], "success", "{loaded}");
    assert!(loaded.to_string().contains(MESSAGE), "{loaded}");
    let old_handle = typescript_call(
        sdk_output.path(),
        &mount,
        &identity,
        "session_refresh_status",
        &refresh,
        None,
        true,
    );
    assert_eq!(old_handle["kind"], "success", "{old_handle}");
    let stale_status = &old_handle["value"]["outcome"]["value"]["payload"];
    assert_eq!(stale_status["outcome"], "stale", "{old_handle}");
    assert_eq!(
        stale_status["error"]["code"], "refresh_handle_stale",
        "{old_handle}"
    );
    assert_eq!(stale_status["scope"], "project", "{old_handle}");
    assert!(stale_status["progress"].is_null(), "{old_handle}");
    assert!(stale_status["receipt"].is_null(), "{old_handle}");
    daemon.kill_and_wait().expect("reap restarted daemon");
}
