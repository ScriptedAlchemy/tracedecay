//! Shared helpers for the MCP server test domains, split mechanically
//! from `mcp_server_test.rs`.

use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_mcp::transport::{ChannelTransport, McpTransport};
use tracedecay_runtime_core::storage::resolve_response_handle_root;

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
    // Boxed server-construction future: the production composition layout
    // overflows the perf-profile test stack when inlined into each test.
    let server = Box::pin(McpServer::new(cg, None)).await;
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
            Box::pin(server.run(&mut transport)).await.unwrap();
        } else {
            Box::pin(server.run_connection(&mut transport))
                .await
                .unwrap();
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

/// A server whose accounting and analytics writes are actually observable.
///
/// [`McpServer::new`] leaves both accounting databases unmounted, so ledger
/// and analytics writes are dropped on the floor. These fields come from one
/// profile this test owns end to end: the server writes through the
/// registered runtime, and `global_db_path` reads the same profile back.
pub(crate) struct AccountedServer {
    pub(crate) server: Arc<McpServer>,
    pub(crate) global_db_path: PathBuf,
    _project: TempDir,
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
    let server = Box::pin(McpServer::new_with_host_admission_test_runtime_for_test(
        cg, None, runtime,
    ))
    .await
    .expect("registered test server");
    AccountedServer {
        server,
        global_db_path,
        _project: dir,
        _profile: profile_dir,
    }
}

pub(crate) async fn mcp_runtime_events(
    global_db_path: &std::path::Path,
    session_id: &str,
) -> Vec<tracedecay_global_db::AnalyticsEventRecord> {
    let runtime = tracedecay::host_admission::HostAdmissionTestRuntimeV1::profile(
        global_db_path
            .parent()
            .expect("global db has a profile root"),
    )
    .await
    .expect("registered profile runtime opens at isolated path");
    runtime
        .query_profile_analytics_events_for_test(&tracedecay_global_db::AnalyticsEventQuery {
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
) -> Option<tracedecay_global_db::AnalyticsEventRecord> {
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
) -> tracedecay_global_db::AnalyticsEventRecord {
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
) -> Vec<tracedecay_global_db::AnalyticsEventRecord> {
    harness
        .read_profile_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
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
) -> tracedecay_global_db::AnalyticsEventRecord {
    harness_mcp_runtime_events(harness, session_id)
        .await
        .into_iter()
        .find(|event| event.tool_name.as_deref() == Some(tool_name))
        .unwrap_or_else(|| panic!("{label}"))
}

#[cfg(feature = "test-transport")]
pub(crate) async fn expect_harness_project_mcp_runtime_event(
    harness: &tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    project_id: &str,
    tool_name: &str,
    label: &str,
) -> tracedecay_global_db::AnalyticsEventRecord {
    harness
        .read_profile_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
            provider: Some("mcp".to_string()),
            project_id: Some(project_id.to_string()),
            session_id: None,
            event_kind: Some("mcp_tool_call".to_string()),
            since: None,
            until: None,
            before_id: None,
            limit: 100,
        })
        .await
        .expect("query project runtime analytics events")
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
    response_with_id(&responses, json!(id))
}

pub(crate) fn analytics_metadata(event: &tracedecay_global_db::AnalyticsEventRecord) -> Value {
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
    crate::common::fixture::git_run(project, args);
}
