//! Daemon-serialized Git index transaction runtime.
//!
//! The public preview/apply facade mounts through the retained service;
//! internal stage/unstage/commit operations remain unreachable by transports.

mod journal;
mod native;
mod owner;
mod queue;
mod recovery;
mod registry;
mod service;
mod store;

use std::path::{Path, PathBuf};

use tracedecay_application::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexPreviewPortResultV1,
    GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1, GitIndexTransactionPort,
    GitIndexTransactionPortError,
};
use tracedecay_domain::{
    GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexPreviewV1, GitIndexTransactionReceiptV1,
    UtcMicros,
};
use tracedecay_policy::GitEffectClassifier;
use tracedecay_store::GitIndexTransactionStore;

/// Canonical filesystem identity for a Git repository/worktree root.
///
/// Preview CAS compares snapshots byte-for-byte. The daemon owner resolves
/// roots through this form before mounting an assembler; snapshot capture must
/// use the same resolution so alias paths (macOS `/tmp` → `/private/tmp`, and
/// other symlink roots) do not produce equal repository state that fails as
/// `stale_preview`. Comparison stays exact — callers must not loosen identity
/// equality to paper over divergent path forms.
pub fn canonicalize_repository_root(repository_root: &Path) -> std::io::Result<PathBuf> {
    repository_root.canonicalize()
}

pub use journal::{DurableGitIndexJournal, GitIndexJournalError};
#[cfg(all(unix, feature = "test-transport"))]
pub use native::capture_exact_snapshot_for_test;
pub use native::{
    DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor, capture_exact_snapshot,
};
pub use owner::{
    DaemonGitAuthorityStateV1, DaemonGitIndexTransactionServiceRegistry, DaemonGitInvocationOwner,
    DaemonProjectGitIndexTransactionService,
};
pub use queue::{RepositoryMutationQueue, RepositoryMutationQueueError};
pub use recovery::{GitIndexRecoveryCoordinator, GitIndexRecoveryError, GitIndexRecoveryExecutor};
pub use registry::GitIndexTransactionStoreRegistry;
pub use service::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionPort, GitIndexNativeExecutor,
    GitIndexPolicyRecheckPort, NativeGitIndexApplyResult,
};
pub use store::{DaemonGitIndexTransactionStore, SharedDaemonGitIndexTransactionStore};

/// The only constructor that makes a daemon Git transaction port available to
/// callers. It creates one queue-owning service, completes durable startup
/// recovery, and exposes no mutation port if recovery fails.
///
/// Startup recovery uses the same queue owned by the published port. The
/// daemon service registry retains exactly one such service per canonical
/// project database.
pub struct DaemonGitIndexTransactionService<S, N, C, A> {
    port: DaemonGitIndexTransactionPort<S, N, C, A>,
}

impl<S, N, C, A> DaemonGitIndexTransactionService<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexRecoveryExecutor,
{
    pub fn save_preview_input(
        &self,
        input: GitIndexPreviewInputV1,
    ) -> Result<(), GitIndexTransactionPortError> {
        self.port.save_preview_input(input)
    }

    pub fn read_preview_input(
        &self,
        preview_id: &GitIndexPreviewId,
        observed_at: UtcMicros,
    ) -> Result<GitIndexPreviewInputV1, GitIndexTransactionPortError> {
        self.port.read_preview_input(preview_id, observed_at)
    }

    pub fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> Result<GitIndexPreviewV1, GitIndexTransactionPortError> {
        self.port.read_preview(preview_id)
    }

    pub fn start(
        store: S,
        native: N,
        classifier: C,
        authorization: A,
        queue: std::sync::Arc<RepositoryMutationQueue>,
        observed_at: UtcMicros,
    ) -> Result<Self, GitIndexTransactionPortError> {
        let port =
            DaemonGitIndexTransactionPort::new(store, native, classifier, authorization, queue);
        port.recover_startup(observed_at)?;
        Ok(Self { port })
    }

    #[cfg(test)]
    pub fn mutation_queue_for_test(&self) -> &std::sync::Arc<RepositoryMutationQueue> {
        self.port.mutation_queue_for_test()
    }
}

impl<S, N, C, A> DaemonGitIndexTransactionService<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    pub fn apply_cancellable(
        &self,
        request: &GitIndexApplyRequestV1,
        cancellation_requested: impl Fn() -> Option<UtcMicros>,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.port.apply_cancellable(request, cancellation_requested)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
    pub fn quarantine_preview_for_test(
        &self,
        preview: &tracedecay_domain::GitIndexPreviewV1,
        observed_at: UtcMicros,
    ) -> Result<(), GitIndexTransactionPortError> {
        self.port.quarantine_preview_for_test(preview, observed_at)
    }
}

impl<S, N, C, A> GitIndexTransactionPort for DaemonGitIndexTransactionService<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        self.port.preview(request)
    }

    fn apply(
        &self,
        request: &GitIndexApplyRequestV1,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.port.apply(request)
    }

    fn recover(
        &self,
        request: &GitIndexRecoveryRequestV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexTransactionPortError> {
        self.port.recover(request)
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;
