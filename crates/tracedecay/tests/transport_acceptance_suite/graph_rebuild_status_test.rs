// Deeply nested async fixture bodies exceed rustc's default layout query
// depth under the perf profile; match the workspace-standard limit used by
// the tracedecay lib and CLI crate roots.

//! Production-boundary coverage for retained code-index publication and reopen.
//!
//! The retired relational graph-rebuild facade owned its own checkpoint/status
//! vocabulary. The final-V2 authority is the daemon's sealed code generation:
//! status reports the serving generation while reconciliation is in progress,
//! and only a complete replacement becomes current. Process cancellation and
//! restart during partial work remain covered by the mounted incremental
//! lifecycle journey in `daemon_suite/indexing_lifecycle_test.rs`; this target
//! exercises the complementary public background-refresh, reopen, and MCP
//! status journey without recreating that substrate.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::common::mcp_response::tool_json;

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(90);

/// How often the waits below re-ask the public MCP surface.
///
/// Each `tracedecay_status` call runs the generation census ready-probe, a
/// scheduler freshness read, and branch diagnostics — Git opens and
/// blocking-pool work on the runtime that is also running the reconcile these
/// waits are waiting for. A `yield_now` spin re-entered that path thousands of
/// times a second, so the observer competed with the publication it observes.
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "gc.autoDetach=false"])
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

fn head(project: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .expect("read git HEAD");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(output.stdout)
        .expect("git HEAD is UTF-8")
        .trim()
        .to_owned()
}

async fn status(harness: &ProductionProjectCompositionHarnessV1, project: &Path) -> Value {
    tool_json(
        harness,
        project,
        "tracedecay_status",
        json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_session_ingest": false,
            "include_staleness": false,
        }),
    )
    .await
}

async fn search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    query: &str,
) -> Value {
    tool_json(
        harness,
        project,
        "tracedecay_search",
        json!({"query": query, "limit": 3, "format": "json"}),
    )
    .await
}

fn result_paths(search: &Value) -> Vec<&str> {
    search["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|result| result["display"]["path"].as_str())
        .collect()
}

async fn wait_for_current_generation(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    expected_revision: &str,
    query: &str,
) -> String {
    let mut last_status = Value::Null;
    let mut last_search = Value::Null;
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            last_status = status(harness, project).await;
            let worktree = &last_status["code_index_freshness"]["worktree"];
            let generation = worktree["latest_generation_id"].as_str().map(str::to_owned);
            if last_status["code_index_freshness"]["status"] == "current"
                && worktree["coverage"] == "complete"
                && worktree["staleness_state"] == "fresh"
                && worktree["source_reference"] == "refs/heads/main"
                && worktree["source_revision"] == expected_revision
                && let Some(current_generation) = generation
            {
                last_search = search(harness, project, query).await;
                if last_search["code_generation"].as_str() == Some(current_generation.as_str())
                    && !result_paths(&last_search).is_empty()
                {
                    assert!(
                        last_status.get("code_index_freshness_warning").is_none(),
                        "a current generation must not carry a warming warning: {last_status}"
                    );
                    return current_generation;
                }
            }
            tokio::time::sleep(READINESS_POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for current generation; status={last_status}; search={last_search}"
        )
    })
}

async fn wait_for_background_refresh(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    old_generation: &str,
    old_revision: &str,
) {
    let mut last_status = Value::Null;
    tokio::time::timeout(RECEIPT_TIMEOUT, async {
        loop {
            last_status = status(harness, project).await;
            let worktree = &last_status["code_index_freshness"]["worktree"];
            if last_status["code_index_freshness"]["status"] == "warming"
                && worktree["latest_generation_id"] == old_generation
                && worktree["source_reference"] == "refs/heads/main"
                && worktree["source_revision"] == old_revision
                && worktree["coverage"] == "partial_refresh_in_progress"
                && worktree["staleness_state"] == "refreshing"
            {
                assert!(
                    last_status["code_index_freshness_warning"]
                        .as_str()
                        .is_some_and(|warning| warning.contains("counts are not authoritative")),
                    "warming status omitted its non-authoritative warning: {last_status}"
                );
                let stale = search(harness, project, "before_reopen").await;
                assert_eq!(stale["code_generation"], old_generation, "{stale}");
                assert_eq!(stale["coverage"]["recall"], "partial", "{stale}");
                assert!(
                    result_paths(&stale).contains(&"src/lib.rs"),
                    "the last complete generation stopped serving: {stale}"
                );
                for lane in ["exact", "lexical", "graph"] {
                    assert_eq!(stale["coverage"][lane]["status"], "stale", "{stale}");
                    assert_eq!(
                        stale["coverage"][lane]["generation"], old_generation,
                        "{stale}"
                    );
                }
                return;
            }
            tokio::time::sleep(READINESS_POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("reopen omitted background-refresh status: {last_status}"));
}

fn install_background_batch(isolation_root: &Path, project: &Path) {
    let staging = isolation_root.join("refresh-batch-staging");
    fs::create_dir_all(&staging).expect("background batch staging directory");
    for file_index in 0..768_u32 {
        let mut source = String::new();
        for symbol_index in 0..128_u32 {
            writeln!(
                source,
                "pub fn refresh_probe_{file_index:04}_{symbol_index:03}(input: u32) -> u32 {{ input + {symbol_index} }}"
            )
            .expect("format background source");
        }
        fs::write(staging.join(format!("file_{file_index:04}.rs")), source)
            .expect("write background source");
    }
    fs::rename(&staging, project.join("src/refresh_batch"))
        .expect("atomically install background batch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_refresh_and_reopen_report_only_servable_generations() {
    Box::pin(background_refresh_and_reopen_report_only_servable_generations_inner()).await
}

async fn background_refresh_and_reopen_report_only_servable_generations_inner() {
    let isolation = tempfile::TempDir::new().expect("isolated production root");
    let project = isolation.path().join("project");
    fs::create_dir_all(project.join("src")).expect("project source directory");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn before_reopen() -> &'static str { \"sealed\" }\n",
    )
    .expect("initial source");
    git(&project, &["init", "--quiet", "--initial-branch=main"]);
    commit_all(&project, "initial source");
    let initial_revision = head(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("open initial production composition");
    let initial_generation =
        wait_for_current_generation(&harness, &project, &initial_revision, "before_reopen").await;

    install_background_batch(isolation.path(), &project);
    commit_all(&project, "install background refresh batch");
    let refreshed_revision = head(&project);
    let receipt = tool_json(
        &harness,
        &project,
        "tracedecay_admin_sync",
        json!({"force": true, "format": "json"}),
    )
    .await;
    assert_eq!(receipt["status"], "queued", "refresh receipt: {receipt}");
    assert_eq!(
        receipt["reconcile_scope"], "authoritative_project",
        "refresh escaped the mounted project authority: {receipt}"
    );
    wait_for_background_refresh(&harness, &project, &initial_generation, &initial_revision).await;

    let refreshed_generation = wait_for_current_generation(
        &harness,
        &project,
        &refreshed_revision,
        "refresh_probe_0000_000",
    )
    .await;
    assert_ne!(
        refreshed_generation, initial_generation,
        "the completed refresh must atomically replace the retained generation"
    );
    harness.shutdown().await;

    fs::write(
        project.join("src/after_reopen.rs"),
        "pub fn after_reopen() -> &'static str { \"current\" }\n",
    )
    .expect("post-shutdown source");
    commit_all(&project, "change source while daemon is closed");
    let reopened_revision = head(&project);

    let reopened = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("reopen production composition after an offline change");
    let reopened_generation =
        wait_for_current_generation(&reopened, &project, &reopened_revision, "after_reopen").await;
    assert_ne!(
        reopened_generation, refreshed_generation,
        "reopen must publish the offline source change before reporting current"
    );
    reopened.shutdown().await;

    let unchanged =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
            .await
            .expect("reopen unchanged production composition");
    assert_eq!(
        wait_for_current_generation(&unchanged, &project, &reopened_revision, "after_reopen").await,
        reopened_generation,
        "an unchanged reopen must retain the same sealed generation"
    );
    unchanged.shutdown().await;
}
