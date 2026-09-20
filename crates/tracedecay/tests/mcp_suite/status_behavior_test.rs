//! `tracedecay_status` as an MCP client observes it.
//!
//! Every call is a JSON-RPC `tools/call` on the production composition
//! server, the same entry an agent host uses. Expectations are the literals
//! that call returns for one sealed fixture, not which helpers ran.

#![cfg(feature = "test-transport")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::common::fixture::{git_capture as git_stdout, git_run as git};

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::fixture;
use crate::support::{TestTempDir, test_temp_dir};

const BRANCH: &str = "status-proof";

struct StatusProject {
    harness: ProductionProjectCompositionHarnessV1,
    project_root: PathBuf,
    head: String,
    _isolation: TestTempDir,
}

async fn open_status_project() -> StatusProject {
    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project dir");
    fixture::write_indexed_fixture_sources(&project_root);
    git(&project_root, &["init", "-q", "-b", BRANCH]);
    git(&project_root, &["add", "."]);
    git(
        &project_root,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "status behavior fixture",
        ],
    );
    let head = git_stdout(&project_root, &["rev-parse", "HEAD"]);
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [project_root.clone()],
    ))
    .await
    .expect("production composition harness");
    let project_root = project_root.canonicalize().expect("canonical project root");
    StatusProject {
        harness,
        project_root,
        head,
        _isolation: isolation,
    }
}

fn tool_text(response: tracedecay_mcp::jsonrpc::JsonRpcResponse) -> String {
    assert!(
        response.error.is_none(),
        "tracedecay_status tools/call failed: {response:?}"
    );
    let result = response.result.expect("tools/call result");
    assert!(
        result.get("isError").is_none(),
        "status must not flag a successful call as an error: {result}"
    );
    assert_eq!(result["content"][0]["type"], "text");
    result["content"][0]["text"]
        .as_str()
        .expect("status text")
        .to_owned()
}

async fn call_status(project: &StatusProject, arguments: Value) -> String {
    let response = project
        .harness
        .call_tool(&project.project_root, "tracedecay_status", arguments)
        .await
        .expect("production tools/call");
    tool_text(response)
}

fn parse_status(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_status JSON was not an object: {error}; body={text}")
    })
}

async fn sealed_json_status(project: &StatusProject) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < Duration::from_secs(20) {
        let payload = parse_status(&call_status(project, json!({ "format": "json" })).await);
        let freshness = &payload["code_index_freshness"];
        let graph = &freshness["worktree"]["code_graph_serving"];
        if freshness["status"] == "current" && graph["state"] == "ready" {
            return payload;
        }
        if graph["state"] == "refused" || graph["reason"] == "activation_disabled" {
            panic!("code index refused to serve: {payload}");
        }
        last = payload;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("tracedecay_status did not report a current sealed generation: {last}");
}

#[tokio::test]
async fn tracedecay_status_reports_the_sealed_branch_and_keeps_diagnostics_opt_in() {
    let project = open_status_project().await;
    let root = project.project_root.display().to_string();
    let compact = sealed_json_status(&project).await;
    let markdown = call_status(&project, json!({})).await;
    let detailed = opted_in_status(&project).await;

    assert_eq!(compact["project_root"], json!(root));
    assert_eq!(compact["active_branch"], json!(BRANCH));
    assert_eq!(compact["serving_branch"], json!(BRANCH));
    assert_eq!(compact["graph_statistics"]["state"], "observed");
    assert_eq!(compact["graph_statistics"]["symbol_count"], 6);
    assert_eq!(compact["graph_statistics"]["edge_count"], 4);
    assert_eq!(compact["graph_statistics"]["source_total_bytes"], 365);
    assert_eq!(
        compact["graph_statistics"]["freshness"],
        json!({ "state": "current" })
    );
    assert_eq!(compact["code_index_freshness"]["status"], "current");
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["worktree_root"],
        json!(root)
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["staleness_state"],
        "fresh"
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["coverage"],
        "complete"
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["rebuild_in_flight"],
        false
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["code_graph_serving"],
        json!({ "state": "ready" })
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["source_reference"],
        "refs/heads/status-proof"
    );
    assert_eq!(
        compact["code_index_freshness"]["worktree"]["source_revision"],
        project.head
    );
    assert_eq!(compact["retrieval_serving"]["status"], "serving");
    assert_eq!(compact["retrieval_serving"]["freshness"], "current");
    assert!(compact["retrieval_serving"].get("condition").is_none());
    assert_eq!(compact["schema_convergence"]["status"], "completed");
    assert_eq!(compact["schema_convergence"]["findings"], json!([]));
    assert!(compact.get("code_index_freshness_warning").is_none());
    assert!(compact.get("node_count").is_none());
    assert_eq!(compact["server"]["errors"], 0);
    assert!(compact["server"].get("worktree_mismatch").is_none());

    for key in [
        "branch_diagnostics",
        "storage_health",
        "session_ingest",
        "session_history_catch_up",
        "git_staleness",
        "live_branch",
        "branch_drifted",
        "parent_branch",
    ] {
        assert!(
            compact.get(key).is_none(),
            "compact status must omit {key}: {compact}"
        );
    }

    assert_eq!(detailed["project_root"], json!(root));
    assert_eq!(detailed["active_branch"], json!(BRANCH));
    assert_eq!(detailed["serving_branch"], json!(BRANCH));
    assert_eq!(detailed["current_branch"], json!(BRANCH));
    assert_eq!(detailed["live_branch"], json!(BRANCH));
    assert_eq!(detailed["branch_drifted"], false);
    assert_eq!(detailed["branch_resolution"], "exact");
    assert_eq!(detailed["branch_diagnostics"]["branch_resolution"], "exact");
    assert_eq!(detailed["branch_diagnostics"]["branch_drifted"], false);
    assert_eq!(
        detailed["branch_diagnostics"]["current_branch"],
        json!(BRANCH)
    );
    assert_eq!(detailed["branch_diagnostics"]["warnings"], json!([]));
    assert_eq!(
        detailed["git_staleness"],
        json!({
            "status": "unavailable",
            "reason": "sealed_generation_git_watermark_not_published",
            "message": "the verified code generation does not publish a Git commit watermark",
        })
    );
    assert_eq!(detailed["session_ingest"], empty_cursor_session_ingest());
    assert_eq!(
        detailed["session_history_catch_up"],
        empty_host_session_history()
    );
    assert_eq!(detailed["tracked_branch_count"], 1);
    assert_eq!(
        detailed["storage_health"]["daemon_owner_pid"],
        json!(u64::from(std::process::id()))
    );
    assert_eq!(
        detailed["storage_health"]["writer_owner"]["pid"],
        json!(u64::from(std::process::id()))
    );

    assert_eq!(
        markdown,
        format!(
            "## Project Status\n\
             **active_branch:** status-proof\n\
             **code_index_freshness.status:** current\n\
             **graph_statistics:** {{6 field(s)}}\n\
             **project_root:** {root}\n\
             **retrieval_serving.status:** serving\n\
             **schema_convergence.status:** completed\n\
             **server:** {{14 field(s)}}\n\
             **serving_branch:** status-proof\n"
        )
    );
}

/// Opt-in diagnostics after the host sweeps on an empty isolated home.
///
/// Cursor coverage and the Kimi frontier land on a background sweep, so a
/// single call during that sweep is not the client-visible settled reading.
async fn opted_in_status(project: &StatusProject) -> Value {
    let arguments = json!({
        "format": "json",
        "include_branch_diagnostics": true,
        "include_storage_health": true,
        "include_session_ingest": true,
        "include_staleness": true,
    });
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < Duration::from_secs(20) {
        let detailed = parse_status(&call_status(project, arguments.clone()).await);
        if detailed["session_ingest"] == empty_cursor_session_ingest()
            && detailed["session_history_catch_up"] == empty_host_session_history()
        {
            return detailed;
        }
        last = detailed;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("opt-in status did not settle on the empty-host session readings: {last}");
}

/// Cursor-scoped ingest for a home with no Cursor transcripts.
fn empty_cursor_session_ingest() -> Value {
    json!({
        "observed_providers": [],
        "provider_coverage": [{
            "provider": "cursor",
            "state": "complete",
            "deferred_units": 0,
        }],
        "tracked_transcripts": 0,
        "pending_transcripts": 0,
        "pending_bytes": 0,
        "max_transcript_pending_bytes": 0,
        "last_ingest_unix": null,
    })
}

/// Historical catch-up after every empty-home sweep has reported.
///
/// Kimi publishes a discovery frontier even when `~/.kimi-code` is absent, so
/// it is the only observed provider. OpenCode has no database, so its coverage
/// stays unavailable. The other admitted hosts finish with nothing pending.
fn empty_host_session_history() -> Value {
    json!({
        "status": "warming",
        "coverage": "partial",
        "authority": "daemon",
        "reason": "historical_provider_coverage_incomplete",
        "providers": ["kimi"],
        "provider_coverage": [
            { "provider": "claude", "state": "complete", "deferred_units": 0 },
            { "provider": "codex", "state": "complete", "deferred_units": 0 },
            { "provider": "cursor", "state": "complete", "deferred_units": 0 },
            { "provider": "kimi", "state": "complete", "deferred_units": 0 },
            { "provider": "opencode", "state": "unavailable", "deferred_units": 1 },
        ],
        "unobserved_providers": ["claude", "codex", "cursor", "opencode"],
        "max_transcript_pending_bytes": 0,
        "pending_bytes": 0,
        "pending_transcripts": 0,
        "message": "Historical session recall is partially available while the daemon continues bounded background catch-up.",
    })
}
