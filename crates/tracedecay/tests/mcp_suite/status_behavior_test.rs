//! `tracedecay_status` as an MCP client observes it.
//!
//! Every call is a JSON-RPC `tools/call` on the production composition
//! server, the same entry an agent host uses. Expectations are the literals
//! that call returns for one sealed fixture, not which helpers ran.

#![cfg(feature = "test-transport")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::common::fixture::{git_capture as git_stdout, git_run as git};

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

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
    let project_root = canonical_existing_identity(&project_root).expect("canonical project root");
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

async fn call_status(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    arguments: Value,
) -> String {
    let response = harness
        .call_tool(project_root, "tracedecay_status", arguments)
        .await
        .expect("production tools/call");
    tool_text(response)
}

fn parse_status(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_status JSON was not an object: {error}; body={text}")
    })
}

async fn sealed_json_status(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < Duration::from_secs(20) {
        let payload =
            parse_status(&call_status(harness, project_root, json!({ "format": "json" })).await);
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
    let compact = sealed_json_status(&project.harness, &project.project_root).await;
    let markdown = call_status(&project.harness, &project.project_root, json!({})).await;
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
    assert_eq!(
        compact["github_source"],
        json!({
            "state": "absent",
            "reason": "the checkout has no GitHub origin, or its advisory owner has not mounted in this daemon",
        })
    );

    let worktree_id = &compact["code_index_freshness"]["worktree"]["worktree_id"];
    let generation_id = &compact["graph_statistics"]["generation_id"];
    let memory = &compact["memory"];
    assert_eq!(memory["status"], "nominal", "{memory}");
    assert_eq!(memory["idle_window_seconds"], 600, "{memory}");
    assert_eq!(
        memory["shed_order"],
        json!([
            "superseded_generation",
            "graph_catalog",
            "decoded_generation",
            "graph_engine"
        ])
    );
    assert_eq!(memory["unmeasured_owners"], 0, "{memory}");
    assert_eq!(
        memory["owners"]
            .as_array()
            .expect("memory owners")
            .iter()
            .map(|owner| owner_row(owner, worktree_id, generation_id))
            .collect::<Vec<_>>(),
        serving_owner_rows(),
        "{memory}"
    );

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

    // Each resident owner renders as one JSON bullet under the memory status;
    // its bytes and idle time are live measurements, so the bullet is read
    // back through the same identity projection as the JSON rows.
    let markdown = markdown
        .lines()
        .map(|line| match line.strip_prefix("- ") {
            Some(owner) => {
                let owner: Value = serde_json::from_str(owner).expect("owner bullet JSON");
                format!("- {}\n", owner_row(&owner, worktree_id, generation_id))
            }
            None => format!("{line}\n"),
        })
        .collect::<String>();
    let owner_bullets = serving_owner_rows()
        .iter()
        .map(|row| format!("- {row}\n"))
        .collect::<String>();
    assert_eq!(
        markdown,
        format!(
            "## Project Status\n\
             **active_branch:** status-proof\n\
             **code_index_freshness.status:** current\n\
             **github_source:** {{2 field(s)}}\n\
             **graph_statistics:** {{6 field(s)}}\n\
             **memory.status:** nominal\n\
             {owner_bullets}\
             **project_root:** {root}\n\
             **reset_required_stores:** 0 item(s)\n\
             **retrieval_serving.status:** serving\n\
             **schema_convergence.status:** completed\n\
             **server:** {{14 field(s)}}\n\
             **serving_branch:** status-proof\n"
        )
    );
}

/// One daemon serving two projects: each project's status lists only its own
/// resident owners, and the doctor's daemon-wide memory inventory lists both.
#[tokio::test]
async fn project_status_lists_only_its_own_memory_owners_and_the_doctor_lists_every_project() {
    let isolation = test_temp_dir();
    let roots = ["alpha", "beta"].map(|name| {
        let root = isolation.path().join(name);
        std::fs::create_dir_all(&root).expect("project dir");
        fixture::write_indexed_fixture_sources(&root);
        git(&root, &["init", "-q", "-b", BRANCH]);
        git(&root, &["add", "."]);
        git(
            &root,
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
        canonical_existing_identity(&root).expect("canonical project root")
    });
    let harness = Box::pin(ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        roots.clone(),
    ))
    .await
    .expect("production composition harness");

    let mut worktrees = Vec::new();
    for root in &roots {
        let status = sealed_json_status(&harness, root).await;
        let project_id = harness.project_id(root).await.expect("project id");
        let owners = status["memory"]["owners"]
            .as_array()
            .expect("memory owners");
        assert_eq!(
            owners
                .iter()
                .map(|owner| (owner["project_id"].clone(), owner["kind"].clone()))
                .collect::<Vec<_>>(),
            [
                (json!(project_id), json!("graph_catalog")),
                (json!(project_id), json!("decoded_generation")),
                (json!(project_id), json!("graph_engine")),
            ],
            "{status}"
        );
        worktrees.push(status["code_index_freshness"]["worktree"]["worktree_id"].clone());
    }
    assert_ne!(worktrees[0], worktrees[1]);

    let runtime = harness
        .call_tool(
            &roots[0],
            "tracedecay_runtime",
            json!({ "format": "json", "doctor_report": true }),
        )
        .await
        .expect("production tools/call");
    let runtime = parse_status(&stored_body(&harness, &roots[0], tool_text(runtime)).await);
    let doctor = runtime["doctor_report"].to_string();
    for worktree in &worktrees {
        let worktree = worktree.as_str().expect("worktree id");
        for kind in ["graph_catalog", "decoded_generation", "graph_engine"] {
            assert!(
                doctor.contains(&format!("{kind} of worktree {worktree} holds")),
                "doctor must list {kind} of {worktree}: {doctor}"
            );
        }
    }
    harness.shutdown().await;
}

/// The whole body behind a truncated response envelope, read back through
/// `tracedecay_retrieve` the way a host recovers it; an untruncated body is
/// returned as is.
async fn stored_body(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    text: String,
) -> String {
    let envelope = parse_status(&text);
    if envelope["truncated"] != true {
        return text;
    }
    let mut body = String::new();
    let mut offset = 0;
    loop {
        let response = harness
            .call_tool(
                project_root,
                "tracedecay_retrieve",
                json!({ "handle": envelope["handle"], "format": "json", "offset": offset }),
            )
            .await
            .expect("production tools/call");
        let page = parse_status(&tool_text(response));
        body.push_str(page["content"].as_str().expect("retrieved content"));
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => return body,
        }
    }
}

/// A resident owner reduced to what identifies it. Every owner on a fresh
/// profile belongs to the one sealed worktree and generation; `measured`
/// holds exactly when the owner reported a byte count.
fn owner_row(owner: &Value, worktree_id: &Value, generation_id: &Value) -> Value {
    assert_eq!(&owner["worktree_id"], worktree_id, "{owner}");
    assert_eq!(&owner["generation_id"], generation_id, "{owner}");
    assert_eq!(
        owner["measured"].as_bool(),
        Some(owner["bytes"].is_u64()),
        "{owner}"
    );
    json!({
        "kind": owner["kind"],
        "measured": owner["measured"],
        "protected": owner["protected"],
    })
}

/// The sealed worktree retains its interactive catalog, serving decode, and
/// graph engine, each sized by its owner and protected while the worktree is
/// in use.
fn serving_owner_rows() -> Vec<Value> {
    vec![
        json!({ "kind": "graph_catalog", "measured": true, "protected": true }),
        json!({ "kind": "decoded_generation", "measured": true, "protected": true }),
        json!({ "kind": "graph_engine", "measured": true, "protected": true }),
    ]
}

/// Opt-in diagnostics after the host sweeps on an empty isolated home.
///
/// Cursor coverage and the Kimi frontier land on a background sweep, so a
/// single call during that sweep is not the client-visible settled reading.
/// The full diagnostic outgrows one response frame, so it is read back from
/// the retained body.
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
        let text = call_status(&project.harness, &project.project_root, arguments.clone()).await;
        let detailed =
            parse_status(&stored_body(&project.harness, &project.project_root, text).await);
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
/// Kimi and Pi publish a discovery frontier even when `~/.kimi-code` and
/// `~/.pi` are absent, so they are the only observed providers. OpenCode has
/// no database, so its coverage stays unavailable. The other admitted hosts
/// finish with nothing pending.
fn empty_host_session_history() -> Value {
    json!({
        "status": "warming",
        "coverage": "partial",
        "authority": "daemon",
        "reason": "historical_provider_coverage_incomplete",
        "providers": ["kimi", "pi"],
        "provider_coverage": [
            { "provider": "claude", "state": "complete", "deferred_units": 0 },
            { "provider": "codex", "state": "complete", "deferred_units": 0 },
            { "provider": "cursor", "state": "complete", "deferred_units": 0 },
            { "provider": "kimi", "state": "complete", "deferred_units": 0 },
            { "provider": "opencode", "state": "unavailable", "deferred_units": 1 },
            { "provider": "pi", "state": "complete", "deferred_units": 0 },
        ],
        "unobserved_providers": ["claude", "codex", "cursor", "opencode"],
        "max_transcript_pending_bytes": 0,
        "pending_bytes": 0,
        "pending_transcripts": 0,
        "message": "Historical session recall is partially available while the daemon continues bounded background catch-up.",
    })
}
