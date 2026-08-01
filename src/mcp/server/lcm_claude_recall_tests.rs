//! Live-shaped Claude recall regression coverage.
//!
//! A real four-record Claude transcript is ingested through the production
//! transcript source, projected into the session-temporal store, and then read
//! back through the same MCP tools an agent calls: `tracedecay_lcm_grep`,
//! `tracedecay_lcm_expand_query`, `tracedecay_lcm_load_session`, and
//! `tracedecay_lcm_expand`. Every stored message must be retrievable through
//! every one of those surfaces; omissions are reserved for anchors that truly
//! cannot be resolved.

use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{ObservationScopeV1, ProjectId, SessionId};

use super::McpServer;
use super::message_search_cutover_tests::{MESSAGE_SEARCH_PROJECT_ID, server_with_authorities};
use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::application::observation::ObservationCancellation;
use crate::mcp::transport::JsonRpcRequest;
use crate::sessions::claude::ClaudeSource;

const SESSION: &str = "claude-recall-session";
/// Present in every one of the four transcript records.
const SHARED_TERM: &str = "quicksilver";
/// Present in exactly one record.
const UNIQUE_TERM: &str = "pangolin";

async fn call_tool(server: &McpServer, name: &str, arguments: Value) -> Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({"name": name, "arguments": arguments})),
    };
    let response = server
        .handle_request(&request)
        .await
        .expect("tool call should produce a response");
    let result = response
        .result
        .unwrap_or_else(|| panic!("{name} JSON-RPC error: {:?}", response.error));
    result["content"]
        .as_array()
        .expect("tool content")
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("{name} JSON content: {result}"))
}

/// Four conversational Claude records (two user, two assistant) whose text all
/// contains [`SHARED_TERM`]; the third additionally contains [`UNIQUE_TERM`].
fn write_claude_transcript(home: &std::path::Path, project: &std::path::Path) {
    let dir = home.join(".claude/projects/-claude-recall");
    std::fs::create_dir_all(&dir).expect("transcript directory");
    let cwd = project.to_string_lossy().to_string();
    let rows = [
        json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": SESSION,
            "uuid": "recall-uuid-1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {
                "role": "user",
                "content": format!("first record mentions {SHARED_TERM} plainly"),
            },
        }),
        json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": SESSION,
            "uuid": "recall-uuid-2",
            "parentUuid": "recall-uuid-1",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {
                "id": "msg_recall_2",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "text", "text": format!("second record answers about {SHARED_TERM}")},
                ],
            },
        }),
        json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": SESSION,
            "uuid": "recall-uuid-3",
            "parentUuid": "recall-uuid-2",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "message": {
                "role": "user",
                "content": format!("third record ties {SHARED_TERM} to the {UNIQUE_TERM} report"),
            },
        }),
        json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": SESSION,
            "uuid": "recall-uuid-4",
            "parentUuid": "recall-uuid-3",
            "timestamp": "2026-01-01T00:00:03.000Z",
            "message": {
                "id": "msg_recall_4",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "text", "text": format!("fourth record closes the {SHARED_TERM} thread")},
                ],
            },
        }),
    ];
    let contents = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        dir.join(format!("{SESSION}.jsonl")),
        format!("{contents}\n"),
    )
    .expect("write transcript");
}

async fn ingest_and_project(
    runtime: &HostAdmissionTestRuntimeV1,
    home: &std::path::Path,
    project: &std::path::Path,
) {
    let source = ClaudeSource::with_home(home);
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new(MESSAGE_SEARCH_PROJECT_ID).expect("typed project identity"),
    };
    let stats =
        crate::sessions::claude_observation::ingest_source_with_observations_with_admission(
            &source,
            project,
            scope,
            &runtime.facade(),
            None,
            ObservationCancellation::default(),
        )
        .await
        .expect("claude transcript should ingest through the observation pipeline");
    let stored = runtime
        .session_message_count_for_test(HostAdmissionScope::Project, None)
        .await
        .expect("stored message count");
    assert!(
        stored >= 4,
        "claude ingest must store every record: stats={stats:?} stored={stored}"
    );
    runtime
        .session_temporal_store_for_test(HostAdmissionScope::Project)
        .expect("registered temporal store")
        .materialize_pending_session_refresh_for_test(&SessionId::new(SESSION).expect("session id"))
        .await
        .expect("materialize claude temporal projection");
}

async fn ingested_server() -> (
    Arc<McpServer>,
    TempDir,
    TempDir,
    crate::config::PinnedUserDataDir,
) {
    let (server, dir, pin) = server_with_authorities().await;
    let home = TempDir::new().expect("temp home");
    write_claude_transcript(home.path(), dir.path());
    let runtime = server
        .host_admission_test_runtime_for_test()
        .expect("retained host-admission test runtime");
    ingest_and_project(runtime, home.path(), dir.path()).await;
    (server, dir, home, pin)
}

fn omission_reasons(payload: &Value) -> Vec<String> {
    payload["omissions"]
        .as_array()
        .map(|omissions| {
            omissions
                .iter()
                .map(|omission| omission["reason"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn lcm_grep_returns_every_matching_claude_message() {
    let (server, _dir, _home, _pin) = ingested_server().await;

    let payload = call_tool(
        &server,
        "tracedecay_lcm_grep",
        json!({
            "provider": "claude",
            "query": SHARED_TERM,
            "scope": "session",
            "session_id": SESSION,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;

    let hits = payload["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        hits.len(),
        4,
        "every stored message containing the term must surface: {payload}"
    );
    assert!(
        omission_reasons(&payload).is_empty(),
        "resolvable anchors must never be reported as omissions: {payload}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn lcm_grep_finds_a_term_stored_in_exactly_one_message() {
    let (server, _dir, _home, _pin) = ingested_server().await;

    let payload = call_tool(
        &server,
        "tracedecay_lcm_grep",
        json!({
            "provider": "claude",
            "query": UNIQUE_TERM,
            "scope": "session",
            "session_id": SESSION,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;

    let hits = payload["hits"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        hits.len(),
        1,
        "the single message holding the term must be found: {payload}"
    );
    assert!(
        hits[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(UNIQUE_TERM)),
        "the returned hit must be the matching message: {payload}"
    );
    assert!(
        omission_reasons(&payload).is_empty(),
        "resolvable anchors must never be reported as omissions: {payload}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn lcm_expand_query_returns_every_matching_claude_message() {
    let (server, _dir, _home, _pin) = ingested_server().await;

    let payload = call_tool(
        &server,
        "tracedecay_lcm_expand_query",
        json!({
            "provider": "claude",
            "prompt": format!("what did the session say about {SHARED_TERM}?"),
            "query": SHARED_TERM,
            "session_id": SESSION,
            "max_results": 20,
            "format": "json",
        }),
    )
    .await;

    let blocks = payload["context_blocks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        blocks.len(),
        4,
        "expand_query must expand every matching message: {payload}"
    );
    assert_eq!(
        payload["omitted"], 0,
        "resolvable anchors must never be omitted: {payload}"
    );
    assert!(
        omission_reasons(&payload).is_empty(),
        "resolvable anchors must never be reported as omissions: {payload}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn lcm_expand_reads_every_live_raw_message_store_id() {
    let (server, _dir, _home, _pin) = ingested_server().await;

    let loaded = call_tool(
        &server,
        "tracedecay_lcm_load_session",
        json!({
            "provider": "claude",
            "session_id": SESSION,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;
    let messages = loaded["messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        messages.len(),
        4,
        "load_session must return the whole stored session: {loaded}"
    );

    let runtime = server
        .host_admission_test_runtime_for_test()
        .expect("retained host-admission test runtime");
    let store_ids = runtime
        .lcm_raw_message_store_ids_for_test(HostAdmissionScope::Project, "claude", SESSION)
        .await
        .expect("live raw message store ids");
    assert_eq!(
        store_ids.len(),
        4,
        "every ingested record must have a live raw-message row"
    );

    for store_id in store_ids {
        let payload = call_tool(
            &server,
            "tracedecay_lcm_expand",
            json!({
                "provider": "claude",
                "session_id": SESSION,
                "target": {"kind": "raw_message", "store_id": store_id},
                    "format": "json",
            }),
        )
        .await;
        assert_eq!(
            payload["status"], "ok",
            "a live raw message must expand, not report deletion: {payload}"
        );
        assert!(
            payload["expansion"]["content"]
                .as_str()
                .is_some_and(|content| content.contains(SHARED_TERM)),
            "expanded content must be the stored message body: {payload}"
        );
    }
    server.shutdown().await;
}
