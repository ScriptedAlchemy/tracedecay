//! Daemon-serialized PR11 Git index transaction runtime.
//!
//! PR11 intentionally has no PR12 transport binding yet, so production entry
//! points remain internally unreferenced while their real-repository and
//! recovery contracts are exercised in this module's tests.

#![allow(dead_code, unused_imports)]

mod journal;
mod native;
mod queue;
mod recovery;
mod service;
mod store;

pub(crate) use journal::{DurableGitIndexJournal, GitIndexJournalError};
pub(crate) use native::{
    FixedDaemonGitIndexExecutor, GitIndexPreviewAssembler, MaterializedGitIndexPreview,
};
pub(crate) use queue::{RepositoryMutationQueue, RepositoryMutationQueueError};
pub(crate) use recovery::{
    GitIndexRecoveryCoordinator, GitIndexRecoveryError, GitIndexRecoveryExecutor,
};
pub(crate) use service::{
    CurrentGitIndexPolicyStateV1, DaemonGitIndexTransactionPort, GitIndexNativeExecutor,
    GitIndexPolicyRecheckPort, NativeGitIndexApplyResult,
};
pub(crate) use store::{
    GIT_INDEX_TRANSACTION_STORE_SCHEMA_VERSION, PersistentGitIndexTransactionStore,
};

#[cfg(test)]
mod tests;
