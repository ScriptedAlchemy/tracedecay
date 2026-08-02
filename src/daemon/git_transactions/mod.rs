//! Daemon-serialized PR11 Git index transaction runtime.
//!
//! PR12 mounts the public preview/apply facade through the retained service;
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
use tracedecay_domain::{GitIndexTransactionReceiptV1, UtcMicros};
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
pub(crate) fn canonicalize_repository_root(repository_root: &Path) -> std::io::Result<PathBuf> {
    repository_root.canonicalize()
}

pub(crate) use journal::{DurableGitIndexJournal, GitIndexJournalError};
#[cfg(all(unix, any(test, feature = "test-transport")))]
pub(crate) use native::capture_exact_snapshot_for_test;
pub(crate) use native::{DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor};
pub(crate) use owner::{
    DaemonGitAuthorityStateV1, DaemonGitIndexTransactionServiceRegistry, DaemonGitInvocationOwner,
    DaemonProjectGitIndexTransactionService,
};
pub(crate) use queue::{RepositoryMutationQueue, RepositoryMutationQueueError};
pub(crate) use recovery::{
    GitIndexRecoveryCoordinator, GitIndexRecoveryError, GitIndexRecoveryExecutor,
};
pub(crate) use registry::GitIndexTransactionStoreRegistry;
pub(crate) use service::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionPort, GitIndexNativeExecutor,
    GitIndexPolicyRecheckPort, NativeGitIndexApplyResult,
};
pub(crate) use store::{DaemonGitIndexTransactionStore, SharedDaemonGitIndexTransactionStore};

/// The only constructor that makes a daemon Git transaction port available to
/// callers. It creates one queue-owning service, completes durable startup
/// recovery, and exposes no mutation port if recovery fails.
///
/// Startup recovery uses the same queue owned by the published port. The
/// daemon service registry retains exactly one such service per canonical
/// project database.
pub(crate) struct DaemonGitIndexTransactionService<S, N, C, A> {
    port: DaemonGitIndexTransactionPort<S, N, C, A>,
}

impl<S, N, C, A> DaemonGitIndexTransactionService<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexRecoveryExecutor,
{
    pub(crate) fn start(
        store: S,
        native: N,
        classifier: C,
        authorization: A,
        observed_at: UtcMicros,
    ) -> Result<Self, GitIndexTransactionPortError> {
        let port = DaemonGitIndexTransactionPort::new(store, native, classifier, authorization);
        port.recover_startup(observed_at)?;
        Ok(Self { port })
    }
}

impl<S, N, C, A> DaemonGitIndexTransactionService<S, N, C, A>
where
    S: GitIndexTransactionStore,
    N: GitIndexNativeExecutor + GitIndexRecoveryExecutor,
    C: GitEffectClassifier,
    A: GitIndexPolicyRecheckPort,
{
    pub(crate) fn apply_cancellable(
        &self,
        request: &GitIndexApplyRequestV1,
        cancellation_requested: impl Fn() -> Option<UtcMicros>,
    ) -> Result<GitIndexApplyPortResultV1, GitIndexTransactionPortError> {
        self.port.apply_cancellable(request, cancellation_requested)
    }

    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
    pub(crate) fn quarantine_preview_for_test(
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
