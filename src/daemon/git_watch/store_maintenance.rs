//! Store-maintenance operations performed by the daemon git watcher.
//!
//! Every operation that opens, tracks, or garbage-collects a store lives here
//! so its [`StoreAdministration`] lifetime is kept separate from the watcher
//! state machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::branch::BranchAdminAction;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use super::super::{branch_admin::StoreAdministration, log_daemon_event};
use super::GitWatcherInner;

/// Opens the project store and runs a diff-scoped incremental sync (or a full
/// sync when the diff base is missing / oversized). Returns true on success.
/// `SyncLock` is treated as success (a peer synced).
///
/// The `TraceDecay` sync/open futures are `Send` (the sync path scopes its
/// `!Send` `gix` values so they drop before every `.await`; see
/// `indexing::stamp_last_synced_commit`), so this awaits them directly on the
/// caller's task under the daemon-wide sync semaphore — no nested runtime.
pub(super) async fn sync_project(
    root: &Path,
    opts: &TraceDecayOpenOptions,
    escalation: usize,
    administration: &StoreAdministration,
) -> bool {
    // Hold the administration gate from before opening the store until the
    // `TraceDecay` handle drops. This prevents branch-store GC from selecting
    // or unlinking the SQLite family while a watcher sync owns it.
    administration
        .with_writer(|| async {
            let Ok(cg) = TraceDecay::open_with_options(root, opts.clone()).await else {
                return false;
            };
            let base = cg.last_synced_commit().await;
            let result = match base {
                Some(base) => match cg.stale_files_since_commit(&base, escalation) {
                    Some(files) if files.is_empty() => Ok(()),
                    Some(files) => cg.sync_if_stale_silent(&files).await,
                    // Base missing/unreachable or over the escalation limit → full.
                    None => cg.sync().await.map(|_| ()),
                },
                None => cg.sync().await.map(|_| ()),
            };
            let synced = matches!(
                result,
                Ok(()) | Err(crate::errors::TraceDecayError::SyncLock { .. })
            );
            drop(cg);
            synced
        })
        .await
}

/// Proactively tracks a linked worktree's branch. Returns the
/// [`crate::branch::BranchAddOutcome`] name for logging, or `None` on error.
pub(super) async fn track_worktree_branch(
    administration: &StoreAdministration,
    wt_root: PathBuf,
    branch: String,
    opts: TraceDecayOpenOptions,
) -> Option<String> {
    administration
        .with_writer(move || async move {
            match TraceDecay::add_branch_tracking_with_options(&wt_root, &branch, opts).await {
                Ok(outcome) => Some(format!("{outcome:?}")),
                Err(_) => None,
            }
        })
        .await
}

/// Resolves a `worktrees/<name>` leaf to `(worktree_root, branch)` by reading
/// its `gitdir` file and the linked HEAD.
pub(super) fn resolve_worktree(common: &Path, name: &str) -> Option<(PathBuf, String)> {
    let wt_meta = common.join("worktrees").join(name);
    let gitdir_file = wt_meta.join("gitdir");
    let gitdir_raw = std::fs::read_to_string(&gitdir_file).ok()?;
    // `gitdir` points at `<worktree>/.git`; the worktree root is its parent.
    let gitdir = PathBuf::from(gitdir_raw.trim());
    let wt_root = gitdir.parent()?.to_path_buf();
    let branch = crate::branch::current_branch(&wt_root)?;
    Some((wt_root, branch))
}

/// Runs branch-store GC for a project through the daemon administration
/// coordinator, logging what it removed. Returns `false` when layout resolution
/// or administration fails so the backstop keeps the GC cadence eligible for a
/// retry.
pub(super) async fn run_gc(
    inner: &Arc<GitWatcherInner>,
    root: &Path,
    opts: &TraceDecayOpenOptions,
) -> bool {
    // Layout discovery is read-only and deliberately stays outside both writer
    // gates. Only the coordinator performs the destructive administration.
    let data_root = match TraceDecay::try_initialized_store_layout_with_options(root, opts).await {
        Ok(Some(layout)) => layout.data_root,
        Ok(None) => return true,
        Err(error) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", root.display().to_string()),
                    ("reason", "branch_gc_layout_failed".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return false;
        }
    };

    // Preserve the sync-semaphore → administration-gate acquisition order used
    // by sync and worktree tracking. The coordinator owns the writer gate and
    // its process/store-holder safety checks.
    let _permit = inner.sync_semaphore.acquire().await;
    let report = inner
        .administration
        .execute_branch_admin_in_layout(
            root,
            &data_root,
            BranchAdminAction::Gc,
            inner.config.branch_gc_days,
            inner.config.orphan_db_gc_days,
        )
        .await;
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            log_daemon_event(
                "git_watch_degraded",
                &[
                    ("project", root.display().to_string()),
                    ("reason", "branch_gc_deferred".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return false;
        }
    };

    if !report.removed_branches.is_empty() || !report.removed_orphan_dbs.is_empty() {
        log_daemon_event(
            "git_watch_synced",
            &[
                ("project", root.display().to_string()),
                ("action", "gc".to_string()),
                ("removed_tracked", report.removed_branches.len().to_string()),
                (
                    "removed_orphans",
                    report.removed_orphan_dbs.len().to_string(),
                ),
            ],
        );
    }
    true
}
