#![cfg(feature = "test-transport")]

//! `tracedecay_status` as a client observes it: one JSON-RPC `tools/call`
//! against the production MCP server, then the rendered payload.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    CaptureTransport, extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture, retained_envelope_payload, truncated_response_handle,
    wait_for_current_graph,
};

/// Compact status of the shared indexed fixture, plus the opt-in sections a
/// caller asks for by name. Both calls go through `tools/call`.
#[tokio::test]
async fn tracedecay_status_reports_sealed_fixture_through_mcp() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let root = std::fs::canonicalize(&fixture.project_root)
        .expect("canonical fixture root")
        .display()
        .to_string();
    let head = git_head(&fixture.project_root);

    // Omitted `format` is markdown. The suite helper would rewrite that to
    // JSON, so this frame is the arguments a client actually sends.
    let markdown = call_status_as_client(&server, json!({})).await;
    assert_eq!(markdown["content"][0]["type"], "text");
    let markdown = tool_text(&markdown);
    assert_eq!(markdown.lines().next(), Some("## Project Status"));
    assert_eq!(
        markdown_field(markdown, "active_branch"),
        "**active_branch:** master"
    );
    assert_eq!(
        markdown_field(markdown, "serving_branch"),
        "**serving_branch:** master"
    );
    assert_eq!(
        markdown_field(markdown, "code_index_freshness.status"),
        "**code_index_freshness.status:** current"
    );
    assert_eq!(
        markdown_field(markdown, "retrieval_serving.status"),
        "**retrieval_serving.status:** serving"
    );
    assert_eq!(
        markdown_field(markdown, "schema_convergence.status"),
        "**schema_convergence.status:** completed"
    );
    assert_eq!(
        markdown_field(markdown, "project_root"),
        format!("**project_root:** {root}")
    );
    assert!(
        !markdown.lines().any(|line| line == "### Warnings"),
        "a current sealed fixture has no status warning: {markdown}"
    );
    assert!(
        !markdown.contains("branch_diagnostics"),
        "compact markdown must not expand the opt-in branch list: {markdown}"
    );

    let compact = status_json(
        &server,
        json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        }),
    )
    .await;
    assert_eq!(object_keys(&compact), compact_status_keys());
    assert_eq!(compact["project_root"], root);
    assert_eq!(compact["active_branch"], "master");
    assert_eq!(compact["serving_branch"], "master");
    assert_eq!(
        compact["schema_convergence"],
        json!({ "status": "completed", "findings": [] })
    );
    let mut serving = compact["retrieval_serving"].clone();
    let serving_fields = serving.as_object_mut().expect("retrieval_serving object");
    serving_fields.remove("seated_generation_age_seconds");
    serving_fields.remove("last_reconcile_age_seconds");
    assert_eq!(
        serving,
        json!({ "status": "serving", "freshness": "current" })
    );
    assert_eq!(compact["graph_statistics"]["state"], "observed");
    assert_eq!(
        compact["graph_statistics"]["freshness"],
        json!({ "state": "current" })
    );
    assert_eq!(
        compact["graph_statistics"]["generation_id"],
        compact["code_index_freshness"]["worktree"]["latest_generation_id"]
    );
    let generation = compact["graph_statistics"]["generation_id"]
        .as_str()
        .expect("sealed generation id");
    assert!(
        generation.starts_with("generation.v1.") && generation.contains(".00000001."),
        "first sealed generation of a fresh fixture: {generation}"
    );
    assert_eq!(compact["code_index_freshness"]["status"], "current");
    let worktree = &compact["code_index_freshness"]["worktree"];
    assert_eq!(worktree["worktree_root"], root);
    assert_eq!(worktree["source_reference"], "refs/heads/master");
    assert_eq!(worktree["source_revision"], head);
    assert_eq!(worktree["staleness_state"], "fresh");
    assert_eq!(worktree["coverage"], "complete");
    assert_eq!(worktree["rebuild_in_flight"], false);
    assert_eq!(worktree["code_graph_serving"], json!({ "state": "ready" }));
    assert_eq!(worktree["parked"], Value::Null);
    assert_eq!(compact["server"]["errors"], 0);

    let again = status_json(&server, json!({ "format": "json" })).await;
    assert_eq!(
        again["server"]["tool_calls"],
        compact["server"]["tool_calls"]
            .as_u64()
            .expect("tool_calls")
            + 1
    );
    assert_eq!(
        again["server"]["tool_call_counts"]["tracedecay_status"],
        compact["server"]["tool_call_counts"]["tracedecay_status"]
            .as_u64()
            .expect("status call count")
            + 1
    );

    let verbose = status_json(
        &server,
        json!({
            "format": "json",
            "include_branch_diagnostics": true,
            "include_storage_health": true,
            "include_session_ingest": true,
            "include_staleness": true,
        }),
    )
    .await;
    assert_eq!(verbose["branch_drifted"], false);
    assert_eq!(verbose["branch_resolution"], "single_db");
    assert_eq!(verbose["current_branch"], "master");
    assert_eq!(verbose["live_branch"], "master");
    assert_eq!(verbose["tracked_branch_count"], 0);
    assert_eq!(verbose["active_branch"], "master");
    assert_eq!(verbose["serving_branch"], "master");
    assert_eq!(verbose["branch_diagnostics"]["tracking_enabled"], false);
    assert_eq!(
        verbose["branch_diagnostics"]["branch_resolution"],
        "single_db"
    );
    assert_eq!(verbose["branch_diagnostics"]["current_branch"], "master");
    assert_eq!(verbose["branch_diagnostics"]["branches"], json!([]));
    assert_eq!(
        verbose["git_staleness"],
        json!({
            "status": "unavailable",
            "reason": "sealed_generation_git_watermark_not_published",
            "message": "the verified code generation does not publish a Git commit watermark",
        })
    );
    assert_eq!(
        verbose["storage_health"]["daemon_owner_pid"],
        json!(std::process::id())
    );
    assert_eq!(
        verbose["session_ingest"],
        json!({
            "observed_providers": [],
            "provider_coverage": [],
            "tracked_transcripts": 0,
            "pending_transcripts": 0,
            "pending_bytes": 0,
            "max_transcript_pending_bytes": 0,
            "last_ingest_unix": null,
        })
    );
    assert_eq!(
        verbose["session_history_catch_up"],
        json!({
            "status": "unavailable",
            "coverage": "partial",
            "authority": "daemon",
            "reason": "historical_sources_unobserved",
            "providers": [],
            "provider_coverage": [],
            "unobserved_providers": [],
            "max_transcript_pending_bytes": 0,
            "pending_bytes": 0,
            "pending_transcripts": 0,
            "message": "No durable historical source rows or provider frontiers are currently observable.",
        })
    );
}

fn compact_status_keys() -> Vec<&'static str> {
    vec![
        "active_branch",
        "code_index_freshness",
        "graph_statistics",
        "project_root",
        "retrieval_serving",
        "schema_convergence",
        "server",
        "serving_branch",
    ]
}

fn object_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("status object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

fn markdown_field<'a>(text: &'a str, key: &str) -> &'a str {
    let prefix = format!("**{key}:** ");
    text.lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing **{key}:** in {text}"))
}

fn git_head(project: &Path) -> String {
    let output = Command::new(crate::common::git_program())
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .expect("git rev-parse HEAD");
    assert!(
        output.status.success(),
        "git rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git HEAD is utf-8")
        .trim()
        .to_owned()
}

async fn status_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_status", arguments).await;
    assert_eq!(result["content"][0]["type"], "text");
    serde_json::from_str(extract_real_server_text(&result)).expect("status JSON")
}

/// `tools/call` with the arguments given, including an omitted `format`.
async fn call_status_as_client(server: &McpServer, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_status",
            "arguments": arguments,
        }
    });
    let mut transport = CaptureTransport::from_request(request.to_string());
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("MCP tools/call");
    let response: Value = serde_json::from_str(transport.output.trim()).expect("JSON-RPC response");
    assert!(response["error"].is_null(), "{response}");
    let mut result = response["result"].clone();
    if let Some(text) = result["content"][0]["text"].as_str()
        && let Some(handle) = truncated_response_handle(text)
    {
        let mut content = String::new();
        let mut offset = 0_u64;
        loop {
            let retrieved = handle_real_server_tool_call(
                server,
                "tracedecay_retrieve",
                json!({ "handle": handle, "offset": offset }),
            )
            .await;
            let page: Value = serde_json::from_str(extract_real_server_text(&retrieved))
                .expect("retrieved page JSON");
            content.push_str(
                page["content"]
                    .as_str()
                    .unwrap_or_else(|| panic!("retrieved page without content: {page}")),
            );
            if page["has_more"] != Value::Bool(true) {
                break;
            }
            offset = page["next_offset"].as_u64().expect("next_offset");
        }
        result["content"][0]["text"] = Value::String(content);
    }
    if let Some(text) = result["content"][0]["text"].as_str()
        && let Some(payload) = retained_envelope_payload(text)
    {
        result["content"][0]["text"] = Value::String(payload.to_string());
    }
    result
}

fn tool_text(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .expect("MCP text result")
}
