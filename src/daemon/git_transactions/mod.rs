//! Daemon-serialized PR11 Git index transaction runtime.
//!
//! PR11 intentionally has no PR12 transport binding yet, so production entry
//! points remain internally unreferenced while their real-repository and
//! recovery contracts are exercised in this module's tests.

#![allow(dead_code)]

mod journal;
mod native;
mod owner;
mod queue;
mod recovery;
mod registry;
mod service;
mod store;

use tracedecay_application::{
    GitIndexApplyPortResultV1, GitIndexApplyRequestV1, GitIndexPreviewPortResultV1,
    GitIndexPreviewRequestV1, GitIndexRecoveryRequestV1, GitIndexTransactionPort,
    GitIndexTransactionPortError,
};
use tracedecay_domain::{GitIndexTransactionReceiptV1, UtcMicros};
use tracedecay_policy::GitEffectClassifier;
use tracedecay_store::GitIndexTransactionStore;

pub(crate) use journal::{DurableGitIndexJournal, GitIndexJournalError};
pub(crate) use native::{DaemonProjectGitIndexPreviewAssembler, FixedDaemonGitIndexExecutor};
pub(crate) use owner::DaemonGitIndexTransactionServiceRegistry;
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
mod tests;
