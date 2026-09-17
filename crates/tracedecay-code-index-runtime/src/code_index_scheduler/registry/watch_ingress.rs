//! Watcher → scheduler freshness ingress.
//!
//! The git-metadata watcher routes repository frontiers into a mounted
//! worktree scheduler here. The Git/stat probe runs on the blocking pool, while
//! the registry and scheduler are both entered through non-waiting locks so a
//! watcher cannot deadlock with mount or shutdown. Contention and worker loss
//! remain distinct typed retry states.

use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

use super::super::CodeIndexCadenceTriggerV1;
use super::CodeIndexSchedulerRegistryV1;

/// Result of routing one watcher frontier into a mounted scheduler.
///
/// Watchers cannot await the registry map without risking a feedback loop with
/// mount/shutdown. `Busy` is therefore explicit and retryable by the bounded
/// watcher owner rather than silently dropping the frontier. A blocking-worker
/// failure is distinct so it cannot masquerade as ordinary lock contention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStateChangeRequestV1 {
    Accepted,
    Unmounted,
    Busy,
    WorkerUnavailable,
    IdentityMismatch,
}

impl CodeIndexSchedulerRegistryV1 {
    /// Route a watcher freshness probe without blocking the watcher thread on
    /// the async registry map. Structural identity is checked before the probe
    /// can enter the scheduler. A quiet backstop tick must not fabricate source
    /// mutation evidence: the scheduler's cheap Git/stat ladder suppresses it,
    /// while real drift records one coalesced worker wake. Contention remains a
    /// typed retry so the watcher never queues behind a capture.
    pub async fn request_for_root(
        &self,
        identity: &GitRepositoryIdentity,
    ) -> GitStateChangeRequestV1 {
        let registry = self.clone();
        let identity = identity.clone();
        match tokio::task::spawn_blocking(move || registry.request_for_root_blocking(&identity))
            .await
        {
            Ok(request) => request,
            Err(_) => {
                hotpath::gauge!("daemon.code_index.watch.ingress.worker_unavailable_total")
                    .inc(1_u64);
                GitStateChangeRequestV1::WorkerUnavailable
            }
        }
    }

    // Every routing outcome below increments one member of a closed counter
    // set, so a stalled worktree is diagnosable as "the watcher never fired",
    // "every frontier bounced Busy", or "the probe kept proving quiet" without
    // per-frontier logging.
    fn request_for_root_blocking(
        &self,
        identity: &GitRepositoryIdentity,
    ) -> GitStateChangeRequestV1 {
        let Ok(mounted) = self.mounted.try_lock() else {
            hotpath::gauge!("daemon.code_index.watch.ingress.busy_total").inc(1_u64);
            return GitStateChangeRequestV1::Busy;
        };
        let Some(worktree) = mounted.get(&identity.worktree_root) else {
            hotpath::gauge!("daemon.code_index.watch.ingress.unmounted_total").inc(1_u64);
            return GitStateChangeRequestV1::Unmounted;
        };
        let Ok(repository_id) =
            super::super::identity::repository_id_for_common_dir(&identity.common_dir)
        else {
            hotpath::gauge!("daemon.code_index.watch.ingress.identity_mismatch_total").inc(1_u64);
            return GitStateChangeRequestV1::IdentityMismatch;
        };
        let Ok(worktree_id) = super::super::identity::worktree_id_for(&identity.worktree_root)
        else {
            hotpath::gauge!("daemon.code_index.watch.ingress.identity_mismatch_total").inc(1_u64);
            return GitStateChangeRequestV1::IdentityMismatch;
        };
        if worktree.repository_id != repository_id || worktree.worktree_id != worktree_id {
            hotpath::gauge!("daemon.code_index.watch.ingress.identity_mismatch_total").inc(1_u64);
            return GitStateChangeRequestV1::IdentityMismatch;
        }
        let scheduler = std::sync::Arc::clone(&worktree.scheduler);
        let wake = std::sync::Arc::clone(&worktree.wake);
        let pending_wake = std::sync::Arc::clone(&worktree.pending_wake);
        drop(mounted);
        let mut scheduler = match scheduler.try_lock() {
            Ok(scheduler) => scheduler,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                hotpath::gauge!("daemon.code_index.watch.ingress.busy_total").inc(1_u64);
                return GitStateChangeRequestV1::Busy;
            }
        };
        if !scheduler.freshness_probe_requires_reconcile() {
            hotpath::gauge!("daemon.code_index.watch.ingress.quiet_total").inc(1_u64);
            return GitStateChangeRequestV1::Accepted;
        }
        hotpath::gauge!("daemon.code_index.watch.ingress.woke_total").inc(1_u64);
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::GitWatcher);
        // The watcher reported a Git state change and the freshness ladder
        // agreed, so this wake carries observed source movement.
        scheduler.request_background_reconcile_for_observed_change();
        drop(scheduler);
        GitStateChangeRequestV1::Accepted
    }
}
