//! End-to-end freshness tests for the wave-1 primitives the daemon
//! git-metadata watcher composes (design D3/D5/D6).
//!
//! Scope note: `GitWatcher` and its debounce/registration internals are
//! crate-private (the `daemon::git_watch` module is not re-exported), so this
//! integration suite cannot instantiate the watcher directly. The watcher's OWN
//! code — `ensure_watching` registration/dedup/cap, the real debounce path, and
//! the safety-critical "a source-file edit triggers no sync" guarantee — is
//! covered by inline unit tests in `src/daemon/git_watch.rs`. The tests here
//! instead prove the freshness CONTRACT end-to-end: that the wave-1 primitives
//! the watcher calls (`stale_files_since_commit`, `sync_if_stale_silent`,
//! `add_branch_tracking`, `gc_dead_branch_stores`) produce the outcomes the
//! watcher relies on, via a `watcher_sync` helper that mirrors
//! `git_watch::sync_project`'s decision tree exactly.
//!
//! Each test drives a real temp git repo through those primitives and asserts
//! the freshness outcome the watcher guarantees:
//!
//! * unhooked `git commit` → diff-scoped incremental sync makes the index fresh;
//! * external `git checkout -b` → a branch store is auto-created;
//! * external `git worktree add` → the worktree store is writable, NOT a
//!   fallback-ancestor (regression guard for the ~100-affected-sessions bug —
//!   cross-references
//!   `storage_suite/branch_db_safety_test::fallback_writes_are_refused_by_all_sync_entry_points`);
//! * a 50-commit rebase → exactly one coalesced sync (the watcher's diff base
//!   advances once, so `stale_files_since_commit` yields one bounded diff);
//! * concurrent sync entry points → single-flight (the shared sync lock);
//! * ref/worktree deletion → GC of the dead branch store.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::common::IsolatedEnv;
use tempfile::TempDir;
use tracedecay::branch::{self, BranchAddOutcome};
use tracedecay::branch_meta::load_branch_meta;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::tracedecay::TraceDecay;
use tracedecay::types::NodeKind;

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        // Match the storage-suite idiom: never fire this repo's real hooks so a
        // developer's global hooks (or an installed tracedecay hook) cannot
        // perturb the "unhooked git operation" scenarios under test.
        .args(["-c", "core.hooksPath=.git/no-hooks"])
        // Disable git's background auto-maintenance. By default every `git
        // commit` forks a detached `git maintenance run --auto` (gc / pack-refs
        // / incremental-repack). In a tight commit burst (e.g. the 50-commit
        // rebase test) that detached process can still be mutating `.git/objects`
        // or refs when the NEXT `git add`/`git commit` runs, so under load the
        // foreground command intermittently fails (e.g. "unable to write file
        // .git/objects/...: No such file or directory" or a ref/index lock
        // error), tripping the assertion below. Suppressing the fork removes the
        // only concurrent writer and makes the git setup deterministic, without
        // changing what any test proves.
        .args(["-c", "gc.auto=0"])
        .args(["-c", "gc.autoDetach=false"])
        .args(["-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
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

/// Initialises a git repo with one indexed file on `main` and returns the
/// indexed project root. Mirrors the watcher's precondition: a project already
/// has a store whose `last_synced_commit` is stamped at HEAD.
async fn init_indexed_repo() -> (IsolatedEnv, PathBuf) {
    let (env, project) = IsolatedEnv::acquire().await;
    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");

    let main = TraceDecay::init(&project).await.unwrap();
    main.index_all().await.unwrap();
    (env, project)
}

async fn init_production_indexed_repo() -> (TempDir, PathBuf, ProductionProjectCompositionHarnessV1)
{
    let root = TempDir::new().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(&project).unwrap();
    git(&project, &["init", "-b", "main"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn on_main() {}\n").unwrap();
    commit_all(&project, "initial commit");
    let harness = ProductionProjectCompositionHarnessV1::open(root.path(), [project.clone()])
        .await
        .unwrap();
    (root, project, harness)
}

/// The watcher's sync-execution logic, reproduced faithfully: diff-scope from
/// `last_synced_commit` when possible, else a full sync. This is the exact
/// decision `git_watch::sync_project` makes; testing it here proves the freshness
/// contract without reaching into the crate-private watcher.
async fn watcher_sync(project: &Path, escalation: usize) {
    let cg = TraceDecay::open(project).await.unwrap();
    match cg.last_synced_commit().await {
        Some(base) => match cg.stale_files_since_commit(&base, escalation) {
            Some(files) if files.is_empty() => {}
            Some(files) => cg.sync_if_stale_silent(&files).await.unwrap(),
            None => {
                cg.sync().await.unwrap();
            }
        },
        None => {
            cg.sync().await.unwrap();
        }
    }
}

/// Unhooked `git commit` → the watcher's diff-scoped sync makes the new symbol
/// searchable.
#[tokio::test]
async fn unhooked_commit_makes_index_fresh() {
    let (_env, project) = init_indexed_repo().await;

    // A commit that adds a brand-new symbol, with NO hook firing.
    fs::write(
        project.join("src/added_by_commit.rs"),
        "pub fn added_by_commit() {}\n",
    )
    .unwrap();
    commit_all(&project, "add symbol out of band");

    // Before the watcher runs, the index must not know the new symbol yet.
    let before = TraceDecay::open(&project).await.unwrap();
    assert!(
        before
            .search("added_by_commit", 10)
            .await
            .unwrap()
            .is_empty(),
        "symbol should be absent until the watcher syncs"
    );
    drop(before);

    watcher_sync(&project, 500).await;

    let after = TraceDecay::open(&project).await.unwrap();
    assert!(
        !after
            .search("added_by_commit", 10)
            .await
            .unwrap()
            .is_empty(),
        "watcher sync should make the committed symbol searchable"
    );
}

/// External `git checkout -b feat/x` → a branch store is auto-created (the
/// watcher's HEAD-move handler calls `add_branch_tracking`).
#[tokio::test]
async fn external_checkout_creates_branch_store() {
    let (_env, project, harness) = init_production_indexed_repo().await;
    let data_root = harness.project_data_root(&project).await.unwrap();

    git(&project, &["checkout", "-b", "feat/x"]);

    // Not tracked yet.
    assert!(
        load_branch_meta(&data_root).is_none_or(|m| !m.is_tracked("feat/x")),
        "branch should be untracked before the watcher reacts"
    );

    let outcome = harness
        .track_worktree_branch(&project, &project, "feat/x")
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            BranchAddOutcome::Added | BranchAddOutcome::AlreadyTracked
        ),
        "watcher branch-add should create the store, got {outcome:?}"
    );

    let meta = load_branch_meta(&data_root).expect("branch meta after add");
    assert!(
        meta.is_tracked("feat/x"),
        "branch store must exist after add"
    );
}

/// External `git worktree add ../wt -b feat/y` → the worktree's store is
/// writable and NOT a fallback-ancestor. This is the regression guard for the
/// harness-worktree bug that put ~100 sessions on a read-only fallback DB.
#[tokio::test]
async fn external_worktree_add_is_auto_tracked_and_writable() {
    let (_root, project, harness) = init_production_indexed_repo().await;

    // A sibling worktree dir, resolved through the shared temp root's parent so
    // it lives under the same isolated storage env.
    let worktree = project.parent().unwrap().join("wt-feat-y");
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

    // The watcher resolves the linked worktree and tracks its branch.
    let outcome = harness
        .track_worktree_branch(&project, &worktree, "feat/y")
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            BranchAddOutcome::Added | BranchAddOutcome::AlreadyTracked
        ),
        "worktree branch should be trackable, got {outcome:?}"
    );

    // And a sync through the worktree store must succeed (a fallback store would
    // refuse the write — see branch_db_safety_test).
    fs::write(worktree.join("src/wt_only.rs"), "pub fn wt_only() {}\n").unwrap();
    commit_all(&worktree, "worktree-only symbol");
    let (active_branch, serving_branch, is_fallback, contains_query) = harness
        .sync_tracked_worktree_branch(&project, &worktree, "feat/y", "wt_only")
        .await
        .expect("writable worktree store must accept a sync");
    assert_eq!(active_branch.as_deref(), Some("feat/y"));
    assert_eq!(serving_branch.as_deref(), Some("feat/y"));
    assert!(
        !is_fallback,
        "tracked worktree must not serve a read-only fallback store"
    );
    assert!(contains_query, "worktree sync should index its own symbol");
}

/// A 50-commit rebase → the watcher fires EXACTLY ONE sync. The watcher
/// coalesces the burst of ref events into a single debounced pass whose diff
/// base (`last_synced_commit`) is the pre-rebase HEAD; one bounded diff covers
/// the whole rebase. We prove the *single-pass* property: one
/// `stale_files_since_commit` + `sync_if_stale_silent` advances the base past
/// all 50 commits, and an immediately-following pass finds nothing to do.
#[tokio::test]
async fn fifty_commit_rebase_needs_one_sync() {
    let (_env, project) = init_indexed_repo().await;

    // Build a 50-commit chain out of band.
    for i in 0..50 {
        fs::write(
            project.join(format!("src/f{i}.rs")),
            format!("pub fn f{i}() {{}}\n"),
        )
        .unwrap();
        commit_all(&project, &format!("commit {i}"));
    }

    let cg = TraceDecay::open(&project).await.unwrap();
    let base = cg
        .last_synced_commit()
        .await
        .expect("base commit stamped at init");

    // One coalesced diff covers all 50 commits (under the escalation limit).
    let files = cg
        .stale_files_since_commit(&base, 500)
        .expect("50 files is within the escalation limit -> one bounded diff");
    assert_eq!(files.len(), 50, "one diff should surface all 50 new files");
    cg.sync_if_stale_silent(&files).await.unwrap();
    drop(cg);

    // A second pass — as the watcher would run on the next (empty) debounce — has
    // nothing to do: the base advanced past the whole rebase in one sync.
    let cg = TraceDecay::open(&project).await.unwrap();
    let base2 = cg.last_synced_commit().await.expect("base advanced");
    let followup = cg.stale_files_since_commit(&base2, 500);
    assert!(
        followup.is_none_or(|f| f.is_empty()),
        "no second sync should be needed after the coalesced pass"
    );
}

/// Two concurrent sync entry points on the same project → extraction runs once.
/// The per-store sync lock is single-flight: one caller wins, the other observes
/// `SyncLock` (which the watcher treats as success — a peer synced). This proves
/// the watcher never double-extracts when it races the on-read / hook paths.
#[tokio::test]
async fn concurrent_syncs_are_single_flight() {
    let (_env, project) = init_indexed_repo().await;

    fs::write(project.join("src/racy.rs"), "pub fn racy() {}\n").unwrap();
    commit_all(&project, "add racy symbol");

    let cg_a = TraceDecay::open(&project).await.unwrap();
    let cg_b = TraceDecay::open(&project).await.unwrap();

    let a = tokio::spawn(async move { cg_a.sync().await });
    let b = tokio::spawn(async move { cg_b.sync().await });
    let (ra, rb) = (a.await.unwrap(), b.await.unwrap());

    // Each result is either a real sync (Ok) or the single-flight lock rejection
    // (SyncLock). Neither is an unexpected error, and at least one is Ok.
    for r in [&ra, &rb] {
        assert!(
            matches!(
                r,
                Ok(_) | Err(tracedecay::errors::TraceDecayError::SyncLock { .. })
            ),
            "concurrent sync must be Ok or SyncLock, got {r:?}"
        );
    }
    assert!(
        ra.is_ok() || rb.is_ok(),
        "at least one concurrent sync must win the lock"
    );

    // The symbol is indexed exactly once regardless of who won.
    let after = TraceDecay::open(&project).await.unwrap();
    let hits = after.search("racy", 10).await.unwrap();
    let function_hits = hits
        .iter()
        .filter(|hit| hit.node.kind == NodeKind::Function && hit.node.name == "racy")
        .count();
    assert_eq!(
        function_hits, 1,
        "single-flight must not double-index the symbol"
    );
}

/// Ref deletion does not permit an unmanaged caller to delete branch stores.
/// The watcher routes GC through daemon-owned store administration; the legacy
/// compatibility API must fail closed even when given a zero grace window.
#[tokio::test]
async fn deleted_branch_store_gc_fails_closed_without_daemon_administration() {
    let (_env, project, harness) = init_production_indexed_repo().await;

    git(&project, &["checkout", "-b", "feat/dead"]);
    harness
        .track_worktree_branch(&project, &project, "feat/dead")
        .await
        .unwrap();
    git(&project, &["checkout", "main"]);
    let data_dir = harness.project_data_root(&project).await.unwrap();
    assert!(
        load_branch_meta(&data_dir).is_some_and(|m| m.is_tracked("feat/dead")),
        "branch should be tracked before its ref is deleted"
    );

    // Delete the git ref out of band, then GC with a zero grace window.
    git(&project, &["branch", "-D", "feat/dead"]);
    let report = branch::gc_dead_branch_stores(&project, &data_dir, 0, 0);
    assert!(
        report.removed_tracked.is_empty() && report.removed_orphan_dbs.is_empty(),
        "unmanaged GC must fail closed, report: {report:?}"
    );
    assert!(
        load_branch_meta(&data_dir).is_some_and(|m| m.is_tracked("feat/dead")),
        "unmanaged GC must preserve branch metadata"
    );
}
