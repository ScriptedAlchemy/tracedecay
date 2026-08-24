//! Watcher → scheduler freshness ingress.
//!
//! The git-metadata watcher routes repository frontiers into a mounted
//! worktree scheduler here. The route is deliberately synchronous: watchers
//! cannot await the async registry map without risking a feedback loop with
//! mount/shutdown, so contention is surfaced as a typed `Busy` for the bounded
//! watcher owner to retry rather than silently dropping the frontier.

use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

use super::super::{CodeIndexCadenceTriggerV1, DaemonCodeIndexControlV1};
use super::CodeIndexSchedulerRegistryV1;

/// Synchronous result of routing one watcher frontier into a mounted scheduler.
///
/// Watchers cannot await the registry map without risking a feedback loop with
/// mount/shutdown. `Busy` is therefore explicit and retryable by the bounded
/// watcher owner rather than silently dropping the frontier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::daemon) enum GitStateChangeRequestV1 {
    Accepted,
    Unmounted,
    Busy,
    IdentityMismatch,
}

impl CodeIndexSchedulerRegistryV1 {
    /// Route a watcher wake without blocking the watcher thread on the async
    /// registry map. Structural identity is checked before the wake can enter
    /// the scheduler's coalescing slot. The scheduler derives the exact git
    /// frontier through its canonical gix reconciliation.
    pub(in crate::daemon) fn request_for_root(
        &self,
        identity: &GitRepositoryIdentity,
    ) -> GitStateChangeRequestV1 {
        let Ok(mounted) = self.mounted.try_lock() else {
            return GitStateChangeRequestV1::Busy;
        };
        let Some(worktree) = mounted.get(&identity.worktree_root) else {
            return GitStateChangeRequestV1::Unmounted;
        };
        let Ok(repository_id) =
            super::super::identity::repository_id_for_common_dir(&identity.common_dir)
        else {
            return GitStateChangeRequestV1::IdentityMismatch;
        };
        let Ok(worktree_id) = super::super::identity::worktree_id_for(&identity.worktree_root)
        else {
            return GitStateChangeRequestV1::IdentityMismatch;
        };
        if worktree.repository_id != repository_id || worktree.worktree_id != worktree_id {
            return GitStateChangeRequestV1::IdentityMismatch;
        }
        worktree
            .hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        DaemonCodeIndexControlV1::advance(&worktree.epoch);
        Self::note_wake(
            &worktree.pending_wake,
            &worktree.wake,
            CodeIndexCadenceTriggerV1::GitWatcher,
        );
        GitStateChangeRequestV1::Accepted
    }
}
