//! Shared helpers for the MCP server test domains, split mechanically
//! from `mcp_server_test.rs`.

use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::mcp::McpServer;
use tracedecay::mcp::transport::{ChannelTransport, McpTransport};
use tracedecay::storage::resolve_response_handle_root;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Creates a temporary Rust project and returns a direct protocol server.
///
/// Graph journeys use [`crate::support::production_composition_fixture`]
/// because only the daemon composition owns code-index publication and graph
/// read admission. This helper is intentionally limited to transport and
/// non-graph tool behavior.
pub(crate) async fn setup_server() -> (Arc<McpServer>, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        "fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n",
    )
    .unwrap();
    let cg = crate::fixture::init_project_from_template(project)
        .await
        .unwrap();
    let server = McpServer::new(cg, None).await;
    (server, dir)
}

/// The identity a fixture's store was actually opened with.
///
/// This is the authority the runtime and the retained project-graph resolver
/// both key on, and unlike the repo-local identity marker it is always present
/// for a template-seeded fixture.
pub(crate) fn project_id_of(cg: &TraceDecay) -> tracedecay_domain::ProjectId {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("fixture project identity");
    tracedecay_domain::ProjectId::new(project_id).expect("fixture project id is well formed")
}

/// Opens an already-initialized project and returns a server that retains the
/// project's registered session authority.
///
/// Every LCM tool refuses to open a store without one, so a fixture built on
/// [`McpServer::new`] makes each LCM call short-circuit to a typed
/// `unavailable` payload instead of exercising the store. Requires an
/// isolated profile (see [`crate::common::IsolatedEnv`]) because the runtime
/// mounts the ambient profile root.
pub(crate) async fn server_with_session_authority(project: &std::path::Path) -> Arc<McpServer> {
    let cg = TraceDecay::open(project).await.unwrap();
    let project_id = project_id_of(&cg);
    let profile_root = tracedecay::storage::default_profile_root().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(&profile_root, project, project_id)
        .await
        .expect("registered project runtime opens for the isolated profile");
    McpServer::new_with_host_admission_test_runtime_for_test(cg, None, runtime)
        .await
        .expect("registered test server")
}

/// As [`setup_server`], but the returned server retains the project's
/// registered session authority. See [`server_with_session_authority`].
pub(crate) async fn setup_server_with_session_authority() -> (Arc<McpServer>, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        "fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n",
    )
    .unwrap();
    let cg = crate::fixture::init_project_from_template(project)
        .await
        .unwrap();
    cg.checkpoint().await.unwrap();
    drop(cg);
    let server = server_with_session_authority(project).await;
    (server, dir)
}

/// Sends a sequence of JSON-RPC messages to a server, runs it to completion,
/// and returns all non-empty response lines.
pub(crate) async fn run_server_with_messages(
    server: Arc<McpServer>,
    messages: Vec<String>,
) -> Vec<String> {
    drive_messages(server, messages, true).await
}

/// As [`run_server_with_messages`], but leaves the server running when the
/// client disconnects — [`McpServer::run_connection`] is the entry point the
/// daemon uses per client socket. Scenarios where a hook arrives on the host's
/// hook socket and the follow-up tool call arrives on the agent's own socket
/// need this: two independent connections against one live server.
pub(crate) async fn run_client_connection_with_messages(
    server: Arc<McpServer>,
    messages: Vec<String>,
) -> Vec<String> {
    drive_messages(server, messages, false).await
}

async fn drive_messages(
    server: Arc<McpServer>,
    messages: Vec<String>,
    shutdown_on_exit: bool,
) -> Vec<String> {
    let (mut transport, sender, mut receiver) = ChannelTransport::new();

    for msg in messages {
        sender.send(msg).unwrap();
    }
    drop(sender);

    let handle = tokio::spawn(async move {
        if shutdown_on_exit {
            server.run(&mut transport).await.unwrap();
        } else {
            server.run_connection(&mut transport).await.unwrap();
        }
    });

    let mut responses = Vec::new();
    while let Some(line) = receiver.recv().await {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() {
            responses.push(trimmed);
        }
    }
    handle.await.unwrap();
    responses
}

/// Helper to build a JSON-RPC request string.
pub(crate) fn jsonrpc_request(id: Value, method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .unwrap()
}

pub(crate) fn response_handle_dir(cg: &TraceDecay) -> PathBuf {
    resolve_response_handle_root(cg.project_root())
        .unwrap_or_else(|err| panic!("failed to resolve test response handle root: {err}"))
}

/// Helper to build a JSON-RPC notification string (no id).
pub(crate) fn jsonrpc_notification(method: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method
    }))
    .unwrap()
}

pub(crate) fn jsonrpc_notification_with_params(method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params
    }))
    .unwrap()
}

/// Parses a JSON-RPC response and returns it.
pub(crate) fn parse_response(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

pub(crate) fn extract_tool_text(value: &Value) -> &str {
    value["content"][0]["text"]
        .as_str()
        .unwrap_or("<missing text>")
}

/// Returns a `tools/call` response's first text block, requiring the call to
/// have succeeded and produced text.
///
/// [`extract_tool_text`] substitutes `<missing text>` when the response has no
/// text content, which lets an errored or empty response satisfy a
/// "must not contain the other project's marker" assertion. Routing tests
/// assert on both presence and absence, so they need the failure to be loud
/// and to carry the whole response.
pub(crate) fn successful_tool_text<'a>(response: &'a Value, label: &str) -> &'a str {
    assert!(
        response["error"].is_null(),
        "{label} must not return a JSON-RPC error: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{label} must return tool text content: {response}"))
}

pub(crate) fn response_with_id(responses: &[String], id: Value) -> Value {
    responses
        .iter()
        .map(|r| parse_response(r))
        .find(|resp| resp.get("id") == Some(&id))
        .unwrap_or_else(|| panic!("response with id {id}"))
}

pub(crate) struct ReadErrorTransport;

impl McpTransport for ReadErrorTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "synthetic read failure",
        ))
    }

    async fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serializes tests that mutate process-wide global-accounting environment.
pub(crate) static SAVINGS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Awaits the server's ledger-write settlement signal (the rows are written
/// by spawned fire-and-forget tasks), then reads the ledger once and asserts
/// the expected row count for `project`. Settlement replaces a wall-clock
/// deadline race against the spawned write task, and the profile belongs to
/// this test alone, so no concurrent fixture can add rows to it.
pub(crate) async fn settled_ledger_total(
    server: &McpServer,
    global_db_path: &std::path::Path,
    project: &std::path::Path,
    expected_calls: u64,
) -> tracedecay::global_db::SavingsTotal {
    server.ledger_writes_settled().await;
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::profile(
        global_db_path
            .parent()
            .expect("global db has a profile root"),
    )
    .await
    .expect("registered profile runtime opens at isolated path");
    let total = runtime
        .sum_savings_for_test(Some(&project.to_string_lossy()), 0)
        .await;
    assert_eq!(
        total.calls, expected_calls,
        "every settled ledger write for this project must be visible (got {} calls)",
        total.calls
    );
    total
}

/// A server whose accounting and analytics writes are actually observable.
///
/// [`McpServer::new`] leaves both accounting databases unmounted, so ledger
/// and analytics writes are dropped on the floor. These fields come from one
/// profile this test owns end to end: the server writes through the
/// registered runtime, and `global_db_path` reads the same profile back.
pub(crate) struct AccountedServer {
    pub(crate) server: Arc<McpServer>,
    pub(crate) project: TempDir,
    pub(crate) global_db_path: PathBuf,
    _profile: TempDir,
}

/// `src/main.rs` large enough that the raw-file ("before") estimate is
/// clearly nonzero.
pub(crate) fn savings_project_source() -> String {
    let mut source = String::from("fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n");
    for i in 0..80 {
        let _ = write!(
            source,
            "/// Filler documentation line {i} to inflate the raw-file estimate.\nfn filler_{i}() -> i32 {{ {i} }}\n"
        );
    }
    source
}

pub(crate) async fn setup_accounted_server() -> AccountedServer {
    setup_accounted_server_with_source(
        "fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n",
    )
    .await
}

pub(crate) async fn setup_accounted_savings_server() -> AccountedServer {
    setup_accounted_server_with_source(&savings_project_source()).await
}

async fn setup_accounted_server_with_source(source: &str) -> AccountedServer {
    let profile_dir = TempDir::new().unwrap();
    // A sibling of the profile root is the isolated transcript-source home
    // the test runtime requires, so the profile cannot be the temp root.
    let profile_root = profile_dir.path().join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), source).unwrap();
    let cg = crate::fixture::init_project_from_template_with_options(
        project,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(global_db_path.clone()),
        },
    )
    .await
    .unwrap();
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
        .expect("savings fixture project identity");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(&profile_root, project, project_id)
        .await
        .expect("registered project runtime opens for the savings profile");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(cg, None, runtime)
        .await
        .expect("registered test server");
    AccountedServer {
        server,
        project: dir,
        global_db_path,
        _profile: profile_dir,
    }
}

/// Extracts `(before, after)` from the `tracedecay_metrics:` line appended
/// to a tool response's content array.
pub(crate) fn parse_metrics_line(resp: &Value) -> Option<(u64, u64)> {
    let content = resp["result"]["content"].as_array()?;
    let line = content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find(|t| t.contains("tracedecay_metrics: before="))?;
    let tail = line.split("before=").nth(1)?;
    let (before, rest) = tail.split_once(' ')?;
    let after = rest.strip_prefix("after=")?;
    Some((before.trim().parse().ok()?, after.trim().parse().ok()?))
}

pub(crate) async fn mcp_runtime_events(
    global_db_path: &std::path::Path,
    session_id: &str,
) -> Vec<tracedecay::global_db::AnalyticsEventRecord> {
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::profile(
        global_db_path
            .parent()
            .expect("global db has a profile root"),
    )
    .await
    .expect("registered profile runtime opens at isolated path");
    runtime
        .query_profile_analytics_events_for_test(&tracedecay::global_db::AnalyticsEventQuery {
            provider: Some("mcp".to_string()),
            project_id: None,
            session_id: Some(session_id.to_string()),
            event_kind: Some("mcp_tool_call".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 100,
        })
        .await
        .expect("query runtime analytics events")
}

pub(crate) async fn mcp_runtime_event(
    global_db_path: &std::path::Path,
    tool_name: &str,
    session_id: &str,
) -> Option<tracedecay::global_db::AnalyticsEventRecord> {
    mcp_runtime_events(global_db_path, session_id)
        .await
        .into_iter()
        .find(|event| event.tool_name.as_deref() == Some(tool_name))
}

pub(crate) async fn expect_mcp_runtime_event(
    global_db_path: &std::path::Path,
    tool_name: &str,
    session_id: &str,
    label: &str,
) -> tracedecay::global_db::AnalyticsEventRecord {
    mcp_runtime_event(global_db_path, tool_name, session_id)
        .await
        .unwrap_or_else(|| panic!("{label}"))
}

/// As [`mcp_runtime_events`], but through the production composition's
/// retained profile authority — a second profile-scoped test runtime cannot
/// open the daemon-owned profile stores.
#[cfg(feature = "test-transport")]
pub(crate) async fn harness_mcp_runtime_events(
    harness: &tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    session_id: &str,
) -> Vec<tracedecay::global_db::AnalyticsEventRecord> {
    harness
        .read_profile_analytics_events(&tracedecay::global_db::AnalyticsEventQuery {
            provider: Some("mcp".to_string()),
            project_id: None,
            session_id: Some(session_id.to_string()),
            event_kind: Some("mcp_tool_call".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 100,
        })
        .await
        .expect("query runtime analytics events")
}

#[cfg(feature = "test-transport")]
pub(crate) async fn expect_harness_mcp_runtime_event(
    harness: &tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    tool_name: &str,
    session_id: &str,
    label: &str,
) -> tracedecay::global_db::AnalyticsEventRecord {
    harness_mcp_runtime_events(harness, session_id)
        .await
        .into_iter()
        .find(|event| event.tool_name.as_deref() == Some(tool_name))
        .unwrap_or_else(|| panic!("{label}"))
}

pub(crate) async fn mcp_runtime_event_count(
    global_db_path: &std::path::Path,
    session_id: &str,
) -> u64 {
    mcp_runtime_events(global_db_path, session_id).await.len() as u64
}

pub(crate) fn response_for_id(responses: &[String], id: i64) -> Value {
    let response = responses
        .iter()
        .find(|r| parse_response(r)["id"] == id)
        .unwrap_or_else(|| panic!("should have a response for id={id}"));
    parse_response(response)
}

pub(crate) async fn call_tool(
    server: Arc<McpServer>,
    id: i64,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(id),
            "tools/call",
            json!({ "name": tool_name, "arguments": arguments }),
        )],
    )
    .await;
    response_for_id(&responses, id)
}

pub(crate) fn analytics_metadata(event: &tracedecay::global_db::AnalyticsEventRecord) -> Value {
    serde_json::from_str(
        event
            .metadata_json
            .as_deref()
            .expect("analytics event metadata"),
    )
    .expect("analytics event metadata is JSON")
}

// ---------------------------------------------------------------------------
// Repository setup used by routed hook journeys.
// ---------------------------------------------------------------------------
pub(crate) fn git(project: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
