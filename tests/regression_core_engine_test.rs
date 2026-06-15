//! Regression tests for adversarial core-engine review findings.
//!
//! Each test mirrors a reproduced bug so a future regression fails loudly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use tempfile::TempDir;
use tracedecay::tracedecay::TraceDecay;

/// Finds the node ID for a symbol by name, panicking if not found.
async fn find_node_id(cg: &TraceDecay, name: &str) -> String {
    let results = cg.search(name, 20).await.unwrap();
    results
        .iter()
        .find(|r| r.node.name == name)
        .unwrap_or_else(|| panic!("node '{name}' not found in index"))
        .node
        .id
        .clone()
}

/// Returns true if `caller` is a (transitive) caller of `target`.
async fn caller_present(cg: &TraceDecay, target: &str, caller: &str) -> bool {
    let target_id = find_node_id(cg, target).await;
    let callers = cg.get_callers(&target_id, 3).await.unwrap();
    callers.iter().any(|(node, _)| node.name == caller)
}

// ---------------------------------------------------------------------------
// Finding #1 (CRITICAL): incremental sync drops incoming cross-file edges to
// edited files.
//
// Repro: two-file repo where `b` calls `a`. Edit `a`'s body and sync. Before
// the fix, `delete_nodes_by_file` removed the unchanged caller's edge into the
// edited file, and because `index_all` never persisted unresolved refs the
// edge was never rebuilt — so `callers(a)` returned empty.
// ---------------------------------------------------------------------------

/// Exact repro: caller indexed by the full `index_all`, then the callee body
/// is edited and synced. The caller edge must survive.
#[tokio::test]
async fn sync_keeps_caller_edge_after_editing_target_indexed_by_index_all() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // `a` defines the target; `b` calls it.
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::a::target_fn;\npub fn caller_fn() -> u32 { target_fn() }\n",
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Baseline: the caller edge exists right after the full index.
    assert!(
        caller_present(&cg, "target_fn", "caller_fn").await,
        "baseline: caller_fn should call target_fn after index_all"
    );

    // Edit only the target's body — the unchanged caller still references it.
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 99 }\n",
    )
    .unwrap();
    cg.sync().await.unwrap();

    assert!(
        caller_present(&cg, "target_fn", "caller_fn").await,
        "sync after editing the target must NOT drop the unchanged caller's edge"
    );
}

/// Variant with two unchanged callers indexed by the full `index_all`. Editing
/// the shared target must keep both caller edges and must not duplicate edges.
#[tokio::test]
async fn sync_keeps_all_caller_edges_after_editing_shared_target() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(
        project.join("src/lib.rs"),
        "pub mod target;\npub mod caller_a;\npub mod caller_b;\n",
    )
    .unwrap();
    fs::write(
        project.join("src/target.rs"),
        "pub fn shared() -> u32 { 0 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/caller_a.rs"),
        "use crate::target::shared;\npub fn a() -> u32 { shared() }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/caller_b.rs"),
        "use crate::target::shared;\npub fn b() -> u32 { shared() }\n",
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    assert!(caller_present(&cg, "shared", "a").await);
    assert!(caller_present(&cg, "shared", "b").await);
    let edges_before = cg.get_stats().await.unwrap().edge_count;

    // Edit the shared target body only.
    fs::write(
        project.join("src/target.rs"),
        "pub fn shared() -> u32 { 42 }\n",
    )
    .unwrap();
    cg.sync().await.unwrap();

    assert!(
        caller_present(&cg, "shared", "a").await,
        "caller a's edge into the edited target must survive sync"
    );
    assert!(
        caller_present(&cg, "shared", "b").await,
        "caller b's edge into the edited target must survive sync"
    );

    let edges_after = cg.get_stats().await.unwrap().edge_count;
    assert_eq!(
        edges_before, edges_after,
        "rebuilding caller edges must not duplicate edges (before={edges_before}, after={edges_after})"
    );
}

/// Control: when the callers are indexed via incremental `sync` (which already
/// persisted unresolved refs), editing the target must also keep the edges.
/// This guards the second indexing path against regressions.
#[tokio::test]
async fn sync_keeps_caller_edge_when_caller_indexed_by_sync() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    // Start with only the target file, full-indexed.
    fs::write(project.join("src/lib.rs"), "pub mod a;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 1 }\n",
    )
    .unwrap();
    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Add the caller file and index it via incremental sync.
    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::a::target_fn;\npub fn caller_fn() -> u32 { target_fn() }\n",
    )
    .unwrap();
    cg.sync().await.unwrap();
    assert!(caller_present(&cg, "target_fn", "caller_fn").await);

    // Edit the target body and sync.
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 7 }\n",
    )
    .unwrap();
    cg.sync().await.unwrap();

    assert!(
        caller_present(&cg, "target_fn", "caller_fn").await,
        "sync must keep the caller edge regardless of how the caller was indexed"
    );
}

/// Sync-only workflow (no `index_all` at all): the very first `sync` builds the
/// whole index from an empty DB. Editing the target afterwards must still keep
/// the caller edge — exercises the "built from empty marks refs complete" path.
#[tokio::test]
async fn sync_only_workflow_keeps_caller_edge_after_editing_target() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::a::target_fn;\npub fn caller_fn() -> u32 { target_fn() }\n",
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    // No index_all: the first sync builds the entire index.
    cg.sync().await.unwrap();
    assert!(caller_present(&cg, "target_fn", "caller_fn").await);

    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 5 }\n",
    )
    .unwrap();
    cg.sync().await.unwrap();

    assert!(
        caller_present(&cg, "target_fn", "caller_fn").await,
        "sync-only workflow must keep the caller edge after editing the target"
    );
}

/// Eager heal: a clean repo (zero changed files) whose index predates ref
/// persistence must still be healed by `sync()`. Before this fix the heal only
/// ran on the re-index path, so no-op syncs left old indexes unstamped and
/// their dropped cross-file edges missing forever.
#[tokio::test]
async fn noop_sync_eagerly_heals_unstamped_index() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::a::target_fn;\npub fn caller_fn() -> u32 { target_fn() }\n",
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Simulate a pre-fix index: marker absent and ref set empty.
    cg.db()
        .set_metadata("unresolved_refs_persisted", "0")
        .await
        .unwrap();
    cg.db().clear_unresolved_refs().await.unwrap();
    assert!(cg.db().get_unresolved_refs().await.unwrap().is_empty());

    // No file has changed — the sync is otherwise a no-op, but it must heal.
    let result = cg.sync().await.unwrap();
    assert_eq!(result.files_modified, 0, "precondition: no changed files");
    assert_eq!(result.files_added, 0, "precondition: no new files");

    assert!(
        !cg.db().get_unresolved_refs().await.unwrap().is_empty(),
        "eager heal must repopulate the persisted ref set on a no-op sync"
    );
    assert_eq!(
        cg.db()
            .get_metadata("unresolved_refs_persisted")
            .await
            .unwrap()
            .as_deref(),
        Some("1"),
        "eager heal must stamp the marker"
    );

    // At-most-once: with the marker stamped, a second no-op sync must NOT
    // re-run the heal. Clearing the refs makes a re-run observable — they
    // must stay empty.
    cg.db().clear_unresolved_refs().await.unwrap();
    cg.sync().await.unwrap();
    assert!(
        cg.db().get_unresolved_refs().await.unwrap().is_empty(),
        "second no-op sync must be a fast no-op (heal runs at most once)"
    );
}

// ---------------------------------------------------------------------------
// Finding #3 (MEDIUM): branch-name sanitization collisions.
//
// "feature/foo" and "feature_foo" both sanitize to "feature_foo". The empty
// case (".." -> "") would yield a hidden `branches/.db`. add_branch_tracking
// must refuse empty names rather than silently mapping them.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_branch_tracking_refuses_empty_sanitized_name() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() {}\n").unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // ".." sanitizes to "" — must be refused, never mapped to branches/.db.
    let result = tracedecay::branch::add_branch_tracking(project, "..").await;
    assert!(
        result.is_err(),
        "a branch name that sanitizes to empty must be refused, got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Finding #4 (LOW): unresolved_refs must stay bounded across repeated edits.
//
// Per-file refs are pruned on re-index (delete_nodes_by_file), so repeatedly
// editing the same file must not grow the table. (Resolved refs are
// intentionally retained — finding #1 re-resolves from them to rebuild edges.)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn repeated_target_edits_keep_unresolved_refs_bounded() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();

    fs::write(project.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
    fs::write(
        project.join("src/a.rs"),
        "pub fn target_fn() -> u32 { 0 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/b.rs"),
        "use crate::a::target_fn;\npub fn caller_fn() -> u32 { target_fn() }\n",
    )
    .unwrap();

    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();

    // Edit the target body repeatedly; capture the ref count after the first
    // sync and assert it never grows on subsequent identical-shape edits.
    let mut baseline: Option<usize> = None;
    for i in 1..=6u32 {
        fs::write(
            project.join("src/a.rs"),
            format!("pub fn target_fn() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
        cg.sync().await.unwrap();
        let count = cg.db().get_unresolved_refs().await.unwrap().len();
        match baseline {
            None => baseline = Some(count),
            Some(b) => assert_eq!(
                b, count,
                "unresolved_refs must stay bounded across repeated edits (was {b}, now {count} at edit {i})"
            ),
        }
        // The caller edge must also keep surviving every edit.
        assert!(caller_present(&cg, "target_fn", "caller_fn").await);
    }
}

// ---------------------------------------------------------------------------
// Finding #5 (LOW): stale sync-lock reclaim must be atomic and preserve a live
// lock. We can't deterministically force the TOCTOU race, but we assert the
// functional contract the atomic reclaim must keep.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_sync_lock_with_dead_pid_is_reclaimed() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    let lock_path = project.join(".tracedecay/sync.lock");
    // A PID well out of range can never be alive -> the lock is stale.
    fs::write(&lock_path, "4294967294").unwrap();

    let guard = tracedecay::tracedecay::try_acquire_sync_lock(project)
        .expect("a stale lock with a dead PID must be reclaimed");
    assert_eq!(
        fs::read_to_string(&lock_path).unwrap().trim(),
        std::process::id().to_string(),
        "reclaimed lock must hold the current PID"
    );
    drop(guard);
    assert!(
        !lock_path.exists(),
        "dropping the guard must remove the lockfile"
    );
}

#[tokio::test]
async fn live_sync_lock_is_not_reclaimed() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    let lock_path = project.join(".tracedecay/sync.lock");
    // Our own PID is alive -> the lock must be treated as in-progress.
    fs::write(&lock_path, format!("{}", std::process::id())).unwrap();

    assert!(
        tracedecay::tracedecay::try_acquire_sync_lock(project).is_err(),
        "a live lock must not be reclaimed"
    );
}
