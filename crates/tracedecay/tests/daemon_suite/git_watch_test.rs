//! Production-boundary freshness tests for the git-metadata watcher.
//!
//! Private watcher routing is covered beside its implementation; these journeys
//! drive its public scheduler and sealed-generation read boundaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::JsonRpcResponse;
use tracedecay_usecases::retention::code_index_generations::{
    DurablePublicationPointerV1, scoped_code_index_store_root,
};
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
    git(project, &["commit", "-m", message]);
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
async fn indexed_repo() -> (TempDir, PathBuf, ProductionProjectCompositionHarnessV1) {
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project,
        &["config", "user.email", "tracedecay-test@example.com"],
    );
    fs::write(project.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");
    let harness = ProductionProjectCompositionHarnessV1::open(root.path(), [project.clone()])
        .await
        .unwrap();
    (root, project, harness)
}
fn tool_payload(response: &JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "{response:?}");
    let result = response.result.as_ref().expect("tool result");
    assert_ne!(result["isError"], true, "tool effect failed: {result}");
    let text = result["content"][0]["text"].as_str().expect("tool text");
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tool returned invalid JSON: {error}; text={text}"))
}
async fn tool(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    name: &str,
    arguments: Value,
) -> Value {
    tool_payload(
        &harness
            .call_tool(project, name, arguments)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}")),
    )
}
async fn status(harness: &ProductionProjectCompositionHarnessV1, project: &Path) -> Value {
    tool(
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
    tool(
        harness,
        project,
        "tracedecay_search",
        json!({"query": query, "limit": 100, "format": "json"}),
    )
    .await
}
fn symbol_count(payload: &Value, name: &str) -> usize {
    payload["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|result| result["display"]["name"] == name)
        .count()
}
fn generation_index_len(data_root: &Path, project: &Path) -> usize {
    let scope = scoped_code_index_store_root(&data_root.join("code-index-v1"), project);
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &fs::read(scope.join("active-code-generation-v1.json")).expect("active generation pointer"),
    )
    .expect("valid active generation pointer");
    pointer.generation_index.len()
}
async fn request_refresh(harness: &ProductionProjectCompositionHarnessV1, project: &Path) {
    let receipt = tool(
        harness,
        project,
        "tracedecay_admin_sync",
        json!({"force": true, "format": "json"}),
    )
    .await;
    assert_eq!(receipt["status"], "queued", "refresh receipt: {receipt}");
    assert_eq!(
        receipt["reconcile_scope"], "authoritative_project",
        "refresh escaped the mounted project authority: {receipt}"
    );
}
async fn wait_for_symbol(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    reference: &str,
    prior_generation: Option<&str>,
    symbol: &str,
) -> String {
    let revision = head(project);
    let mut last_status = Value::Null;
    let mut last_search = Value::Null;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            last_status = status(harness, project).await;
            let worktree = &last_status["code_index_freshness"]["worktree"];
            let generation = worktree["latest_generation_id"].as_str().map(str::to_owned);
            let current = last_status["code_index_freshness"]["status"] == "current"
                && worktree["coverage"] == "complete"
                && worktree["staleness_state"] == "fresh"
                && worktree["source_reference"] == reference
                && worktree["source_revision"] == revision
                && generation
                    .as_deref()
                    .is_some_and(|generation| prior_generation != Some(generation));
            if current {
                last_search = search(harness, project, symbol).await;
                if last_search["code_generation"].as_str() == generation.as_deref()
                    && symbol_count(&last_search, symbol) == 1
                {
                    return generation.expect("current generation");
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("timed out waiting for {symbol:?}; status={last_status}; search={last_search}")
    })
}
async fn wait_for_stale_serving(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    generation: &str,
    reference: &str,
    revision: &str,
    symbol: &str,
) {
    let mut last = Value::Null;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            last = status(harness, project).await;
            let worktree = &last["code_index_freshness"]["worktree"];
            if last["code_index_freshness"]["status"] == "warming"
                && worktree["latest_generation_id"] == generation
                && worktree["source_reference"] == reference
                && worktree["source_revision"] == revision
                && worktree["staleness_state"] == "refreshing"
                && worktree["coverage"] == "partial_refresh_in_progress"
            {
                let stale = search(harness, project, symbol).await;
                assert_eq!(stale["code_generation"], generation, "{stale}");
                assert_eq!(stale["coverage"]["recall"], "partial", "{stale}");
                for lane in ["exact", "lexical", "graph"] {
                    assert_eq!(stale["coverage"][lane]["status"], "stale", "{stale}");
                    assert_eq!(stale["coverage"][lane]["generation"], generation, "{stale}");
                }
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("refresh omitted stale-serving evidence: {last}"));
}
#[tokio::test]
async fn unhooked_commit_becomes_searchable_through_scheduler_reconciliation() {
    let (_root, project, harness) = indexed_repo().await;
    let initial = wait_for_symbol(&harness, &project, "refs/heads/main", None, "on_main").await;
    fs::write(
        project.join("src/added_by_commit.rs"),
        "pub fn added_by_commit() {}\n",
    )
    .unwrap();
    commit_all(&project, "add symbol out of band");
    assert_eq!(
        symbol_count(
            &search(&harness, &project, "added_by_commit").await,
            "added_by_commit"
        ),
        0
    );
    request_refresh(&harness, &project).await;
    let fresh = wait_for_symbol(
        &harness,
        &project,
        "refs/heads/main",
        Some(&initial),
        "added_by_commit",
    )
    .await;
    assert_ne!(fresh, initial);
    harness.shutdown().await;
}
#[tokio::test]
async fn external_checkout_rebinds_the_exact_sealed_generation() {
    let (_root, project, harness) = indexed_repo().await;
    let initial = wait_for_symbol(&harness, &project, "refs/heads/main", None, "on_main").await;
    git(&project, &["checkout", "-b", "feat/x"]);
    request_refresh(&harness, &project).await;
    let fresh = wait_for_symbol(
        &harness,
        &project,
        "refs/heads/feat/x",
        Some(&initial),
        "on_main",
    )
    .await;
    assert_ne!(fresh, initial);
    assert!(
        !harness
            .project_data_root(&project)
            .await
            .unwrap()
            .join("branches")
            .exists()
    );
    harness.shutdown().await;
}
#[tokio::test]
async fn linked_worktree_requires_mount_then_serves_only_its_exact_generation() {
    let (root, project, harness) = indexed_repo().await;
    let worktree = root.path().join("wt-feat-y");
    git(
        &project,
        &[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "feat/y",
        ],
    );
    let error = harness
        .track_worktree_branch(&project, &worktree, "feat/y")
        .await
        .expect_err("an unmounted worktree must fail closed");
    assert!(format!("{error}").contains("code_index_scheduler_unavailable"));
    harness.shutdown().await;

    let harness = ProductionProjectCompositionHarnessV1::open(
        root.path(),
        [project.clone(), worktree.clone()],
    )
    .await
    .unwrap();
    let main = wait_for_symbol(&harness, &project, "refs/heads/main", None, "on_main").await;
    let initial = wait_for_symbol(&harness, &worktree, "refs/heads/feat/y", None, "on_main").await;
    fs::write(worktree.join("src/wt_only.rs"), "pub fn wt_only() {}\n").unwrap();
    commit_all(&worktree, "worktree-only symbol");
    request_refresh(&harness, &worktree).await;
    wait_for_symbol(
        &harness,
        &worktree,
        "refs/heads/feat/y",
        Some(&initial),
        "wt_only",
    )
    .await;
    let isolated = search(&harness, &project, "wt_only").await;
    assert_eq!(isolated["code_generation"], main);
    assert_eq!(isolated["coverage"]["recall"], "full");
    for lane in ["exact", "lexical", "graph"] {
        assert_eq!(isolated["coverage"][lane], "complete", "{isolated}");
    }
    assert_eq!(symbol_count(&isolated, "wt_only"), 0);
    harness.shutdown().await;
}
#[tokio::test]
async fn one_reconciliation_covers_a_fifty_commit_frontier() {
    let (_root, project, harness) = indexed_repo().await;
    let initial_revision = head(&project);
    let initial = wait_for_symbol(&harness, &project, "refs/heads/main", None, "on_main").await;
    for index in 0..50 {
        let mut source = format!("pub fn f{index}() {{}}\n");
        for symbol in 0..128 {
            source.push_str(&format!("pub fn f{index}_{symbol}() {{}}\n"));
        }
        fs::write(project.join(format!("src/f{index}.rs")), source).unwrap();
        commit_all(&project, &format!("commit {index}"));
    }
    request_refresh(&harness, &project).await;
    wait_for_stale_serving(
        &harness,
        &project,
        &initial,
        "refs/heads/main",
        &initial_revision,
        "on_main",
    )
    .await;
    let fresh = wait_for_symbol(&harness, &project, "refs/heads/main", Some(&initial), "f49").await;
    let first = search(&harness, &project, "f0").await;
    assert_eq!(first["code_generation"], fresh);
    assert_eq!(symbol_count(&first, "f0"), 1);
    harness.shutdown().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reconciliations_converge_on_one_sealed_generation() {
    let (_root, project, harness) = indexed_repo().await;
    let initial = wait_for_symbol(&harness, &project, "refs/heads/main", None, "on_main").await;
    fs::write(project.join("src/racy.rs"), "pub fn racy() {}\n").unwrap();
    commit_all(&project, "add racy symbol");
    let data_root = harness.project_data_root(&project).await.unwrap();
    let before = generation_index_len(&data_root, &project);
    tokio::join!(
        request_refresh(&harness, &project),
        request_refresh(&harness, &project)
    );
    let fresh = wait_for_symbol(
        &harness,
        &project,
        "refs/heads/main",
        Some(&initial),
        "racy",
    )
    .await;
    let (left, right) = tokio::join!(
        search(&harness, &project, "racy"),
        search(&harness, &project, "racy")
    );
    assert_eq!(left["code_generation"], fresh);
    assert_eq!(right["code_generation"], fresh);
    assert_eq!(symbol_count(&left, "racy"), 1);
    assert_eq!(symbol_count(&right, "racy"), 1);
    assert_eq!(generation_index_len(&data_root, &project), before + 1);
    harness.shutdown().await;
}
