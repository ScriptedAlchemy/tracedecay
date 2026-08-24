//! Persistence contract for native integration previews and transactions.
//!
//! Implementations atomically consume approvals, compare-and-swap phase
//! revisions, publish terminal receipts, and enumerate unfinished records for
//! restart recovery. They never open repositories or graph databases.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DomainError, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationApprovalV1,
    NativeIntegrationPreviewId, NativeIntegrationPreviewV1, NativeIntegrationReceiptV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1,
    NativeWorktreeCleanupReceiptV1, NativeWorktreeCleanupTransactionV1, RepositoryId,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeIntegrationStoreError {
    #[error("native integration preview conflicts with immutable stored evidence")]
    PreviewConflict,
    #[error("native integration approval is already consumed or conflicts")]
    ApprovalConflict,
    #[error("native integration transaction already exists with different input")]
    TransactionConflict,
    #[error("native integration status compare-and-set failed")]
    StatusConflict,
    #[error("native integration terminal receipt conflicts")]
    ReceiptConflict,
    #[error("native worktree cleanup transaction conflicts with durable intent")]
    CleanupTransactionConflict,
    #[error("native worktree cleanup terminal receipt conflicts")]
    CleanupReceiptConflict,
    #[error("native integration repository is quarantined")]
    RepositoryQuarantined,
    #[error("native integration store is unavailable")]
    Unavailable,
    #[error("native integration store requires reset")]
    ResetRequired,
    #[error("native integration store durability is uncertain")]
    DurabilityUncertain,
    #[error("native integration stored data is invalid: {0}")]
    InvalidData(String),
}

impl From<DomainError> for NativeIntegrationStoreError {
    fn from(error: DomainError) -> Self {
        Self::InvalidData(error.to_string())
    }
}

pub type NativeIntegrationStoreResult<T> = Result<T, NativeIntegrationStoreError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationRecordV1 {
    pub preview: NativeIntegrationPreviewV1,
    pub approval: NativeIntegrationApprovalV1,
    pub status: NativeIntegrationTransactionStatusV1,
    pub terminal_receipt: Option<NativeIntegrationReceiptV1>,
}

impl NativeIntegrationRecordV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.preview.validate()?;
        self.approval.validate()?;
        self.status.validate()?;
        if self.approval.preview_id != self.preview.preview_id
            || self.approval.preview_digest != self.preview.preview_digest
            || self.status.preview_id != self.preview.preview_id
            || self.status.preview_digest != self.preview.preview_digest
            || self.status.approval_id != self.approval.approval_id
            || self.status.repository_id != self.preview.repository_snapshot.repository_id
            || self.status.destination_ref != self.preview.repository_snapshot.destination_ref
            || self.status.expected_destination_tip
                != self.preview.repository_snapshot.destination_tip
        {
            return Err(DomainError::SnapshotMismatch {
                field: "native integration stored record binding",
            });
        }
        match &self.terminal_receipt {
            Some(receipt) => {
                receipt.validate()?;
                if receipt.status != self.status {
                    return Err(DomainError::SnapshotMismatch {
                        field: "native integration stored terminal receipt",
                    });
                }
            }
            None if self.status.terminal_outcome.is_some() => {
                return Err(DomainError::NonCanonical {
                    field: "native integration terminal record receipt",
                });
            }
            None => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeIntegrationBeginResultV1 {
    Started(Box<NativeIntegrationRecordV1>),
    Replay(Box<NativeIntegrationReceiptV1>),
    RecoveryRequired(Box<NativeIntegrationRecordV1>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeWorktreeCleanupBeginResultV1 {
    Started(Box<NativeWorktreeCleanupTransactionV1>),
    Replay(Box<NativeWorktreeCleanupReceiptV1>),
    RecoveryRequired(Box<NativeWorktreeCleanupTransactionV1>),
}

/// Durable transaction authority.
pub trait NativeIntegrationStore: Send + Sync {
    fn save_preview(&self, preview: NativeIntegrationPreviewV1)
    -> NativeIntegrationStoreResult<()>;

    fn read_preview(
        &self,
        preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>>;

    /// Atomically consumes the approval and inserts the `Prepared` record.
    fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1>;

    fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>>;

    fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>;

    fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>>;

    fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1>;

    /// Atomically publishes the terminal status and its receipt.
    fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1>;

    fn pending_transactions(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>>;

    fn approval_consumed(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool>;

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()>;

    /// Durably records exact cleanup intent before native Git may mutate the
    /// registered worktree. Exact replay returns the terminal receipt or the
    /// in-flight record; a different intent under the confirmation digest is
    /// a conflict.
    fn begin_worktree_cleanup(
        &self,
        transaction: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupBeginResultV1>;

    fn read_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<NativeWorktreeCleanupTransactionV1>>;

    /// Bounded startup recovery census for unfinished cleanup journals in one
    /// exact repository. Implementations must fail rather than truncate.
    fn pending_worktree_cleanups(
        &self,
        _repository_id: &RepositoryId,
        _limit: u32,
    ) -> NativeIntegrationStoreResult<Vec<NativeWorktreeCleanupTransactionV1>> {
        Err(NativeIntegrationStoreError::Unavailable)
    }

    fn compare_and_swap_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        replacement: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupTransactionV1>;

    fn write_worktree_cleanup_terminal(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        receipt: NativeWorktreeCleanupReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupReceiptV1>;
}
