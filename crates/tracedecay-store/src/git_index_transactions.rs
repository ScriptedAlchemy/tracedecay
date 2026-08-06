//! Persistence contracts for daemon-owned Git index transactions.
//!
//! Storage implementations append immutable previews and terminal receipts,
//! compare-and-swap journal phases, and preserve idempotency across process
//! restart. They never open a repository or perform a native Git operation.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitIndexIdempotencyKey, GitIndexJournalPhaseV1, GitIndexPreviewId,
    GitIndexPreviewInputV1, GitIndexPreviewV1, GitIndexReceiptOutcomeV1,
    GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1, ManifestDigest, RepositoryId,
    UtcMicros,
};

pub const MAX_GIT_INDEX_PREVIEW_INPUT_BYTES: usize = 1_048_576;
pub const MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH: usize = 256;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GitIndexTransactionStoreError {
    #[error("git index preview input conflicts with an existing immutable input")]
    PreviewInputConflict,
    #[error("git index preview input exceeds the durable payload budget")]
    PreviewInputTooLarge,
    #[error("git index transaction preview conflicts with an existing immutable preview")]
    PreviewConflict,
    #[error("git index transaction idempotency key conflicts with a prior input")]
    IdempotencyConflict,
    #[error("git index transaction journal compare-and-swap failed")]
    JournalConflict,
    #[error("git index transaction receipt conflicts with an existing terminal receipt")]
    ReceiptConflict,
    #[error("git index transaction repository is quarantined pending inspection")]
    RepositoryQuarantined,
    #[error("git index transaction store is unavailable")]
    Unavailable,
    #[error("git index transaction store data is invalid: {0}")]
    InvalidData(String),
}

impl From<DomainError> for GitIndexTransactionStoreError {
    fn from(error: DomainError) -> Self {
        Self::InvalidData(error.to_string())
    }
}

pub type GitIndexTransactionStoreResult<T> = Result<T, GitIndexTransactionStoreError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum GitIndexPreviewInputReadV1 {
    Available(Box<GitIndexPreviewInputV1>),
    Expired {
        expired_at: UtcMicros,
        purged_at: Option<UtcMicros>,
    },
    Missing,
}

/// Durable record keyed by the application idempotency key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIndexTransactionRecordV1 {
    pub idempotency_key: GitIndexIdempotencyKey,
    pub input_digest: ManifestDigest,
    pub preview: GitIndexPreviewV1,
    pub journal: GitIndexTransactionJournalV1,
    pub terminal_receipt: Option<GitIndexTransactionReceiptV1>,
}

impl GitIndexTransactionRecordV1 {
    /// Verify immutable receipt fields against this record's durable preview.
    /// Terminal phase/timestamp checks remain in `validate` because recovery
    /// proofs are checked against the same immutable binding before terminal
    /// journal publication.
    pub fn receipt_binds_preview(&self, receipt: &GitIndexTransactionReceiptV1) -> bool {
        receipt.validate().is_ok()
            && receipt.transaction_id == self.journal.transaction_id
            && receipt.preview_id == self.preview.preview_id
            && receipt.operation == self.preview.operation
            && receipt.old_snapshot_digest == self.preview.repository_snapshot_digest
            && receipt.old_index_tree == self.preview.repository_snapshot.index.tree_id
            && receipt.old_head == self.preview.repository_snapshot.head.commit().cloned()
            && self
                .preview
                .selected_hunk_digests()
                .is_ok_and(|digests| receipt.selected_hunk_digests == digests)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.idempotency_key.validate()?;
        self.input_digest.validate()?;
        self.preview.validate()?;
        self.journal.validate()?;
        if self.journal.preview_id != self.preview.preview_id
            || self.journal.preview_digest != self.preview.preview_digest
            || self.journal.repository_id != self.preview.repository_snapshot.repository_id
            || self.journal.worktree_id
                != self.preview.repository_snapshot.worktree_id.clone().ok_or(
                    DomainError::NonCanonical {
                        field: "git index transaction record worktree",
                    },
                )?
            || self.journal.operation != self.preview.operation
            || self.journal.expected_snapshot_digest != self.preview.repository_snapshot_digest
        {
            return Err(DomainError::SnapshotMismatch {
                field: "git index transaction record preview binding",
            });
        }

        match &self.terminal_receipt {
            None if self.journal.phase.is_terminal() => Err(DomainError::NonCanonical {
                field: "terminal git index journal receipt",
            }),
            None => Ok(()),
            Some(receipt) => {
                if !self.receipt_binds_preview(receipt)
                    || receipt.committed_at != self.journal.updated_at
                    || !self.journal.phase.is_terminal()
                {
                    return Err(DomainError::SnapshotMismatch {
                        field: "git index transaction terminal receipt binding",
                    });
                }
                let expected_phase = match receipt.outcome {
                    GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
                    GitIndexReceiptOutcomeV1::AbortedNoChange => {
                        GitIndexJournalPhaseV1::AbortedNoChange
                    }
                    GitIndexReceiptOutcomeV1::NeedsInspection => {
                        GitIndexJournalPhaseV1::NeedsInspection
                    }
                };
                if self.journal.phase != expected_phase {
                    return Err(DomainError::SnapshotMismatch {
                        field: "git index transaction receipt journal phase",
                    });
                }
                Ok(())
            }
        }
    }
}

/// Atomic begin-or-replay request. A store must persist the immutable preview
/// and `Prepared` journal together before native state can change.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIndexTransactionBeginRequestV1 {
    pub idempotency_key: GitIndexIdempotencyKey,
    pub input_digest: ManifestDigest,
    pub preview: GitIndexPreviewV1,
    pub journal: GitIndexTransactionJournalV1,
}

impl GitIndexTransactionBeginRequestV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        GitIndexTransactionRecordV1 {
            idempotency_key: self.idempotency_key.clone(),
            input_digest: self.input_digest.clone(),
            preview: self.preview.clone(),
            journal: self.journal.clone(),
            terminal_receipt: None,
        }
        .validate()?;
        if self.journal.phase != GitIndexJournalPhaseV1::Prepared {
            return Err(DomainError::NonCanonical {
                field: "git index transaction begin journal phase",
            });
        }
        Ok(())
    }
}

/// Whether an idempotency key starts a native transaction, returns its already
/// durable terminal receipt, or must be reconciled before it can be used
/// again. A non-terminal record is never permission to replay native Git.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum GitIndexTransactionBeginResultV1 {
    Started(Box<GitIndexTransactionRecordV1>),
    Replay(Box<GitIndexTransactionReceiptV1>),
    RecoveryRequired(Box<GitIndexTransactionRecordV1>),
}

/// One atomic terminal write. The store advances the current non-terminal
/// journal to the matching terminal phase and inserts the immutable receipt in
/// one durable transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIndexTransactionTerminalWriteV1 {
    pub idempotency_key: GitIndexIdempotencyKey,
    pub expected_phase_epoch: u64,
    pub journal: GitIndexTransactionJournalV1,
    pub receipt: GitIndexTransactionReceiptV1,
}

impl GitIndexTransactionTerminalWriteV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.idempotency_key.validate()?;
        self.journal.validate()?;
        self.receipt.validate()?;
        if self.expected_phase_epoch == 0
            || self.journal.phase_epoch != self.expected_phase_epoch
            || self.receipt.transaction_id != self.journal.transaction_id
            || self.receipt.preview_id != self.journal.preview_id
            || self.receipt.operation != self.journal.operation
        {
            return Err(DomainError::SnapshotMismatch {
                field: "git index transaction terminal write binding",
            });
        }
        let expected_phase = match self.receipt.outcome {
            GitIndexReceiptOutcomeV1::Committed => GitIndexJournalPhaseV1::Committed,
            GitIndexReceiptOutcomeV1::AbortedNoChange => GitIndexJournalPhaseV1::AbortedNoChange,
            GitIndexReceiptOutcomeV1::NeedsInspection => GitIndexJournalPhaseV1::NeedsInspection,
        };
        if self.journal.phase != expected_phase {
            return Err(DomainError::SnapshotMismatch {
                field: "git index transaction terminal write phase",
            });
        }
        Ok(())
    }
}

/// Append-only preview/receipt store with a mutable, compare-and-swap journal.
pub trait GitIndexTransactionStore {
    fn save_preview_input(
        &self,
        input: GitIndexPreviewInputV1,
    ) -> GitIndexTransactionStoreResult<()>;

    fn read_preview_input(
        &self,
        preview_id: &GitIndexPreviewId,
        observed_at: UtcMicros,
    ) -> GitIndexTransactionStoreResult<GitIndexPreviewInputReadV1>;

    fn purge_expired_preview_inputs(
        &self,
        observed_at: UtcMicros,
        limit: usize,
    ) -> GitIndexTransactionStoreResult<usize>;

    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()>;

    fn read_preview(
        &self,
        preview_id: &tracedecay_domain::GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>>;

    fn read_record(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>>;

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1>;

    fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1>;

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1>;

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>>;

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>>;

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &tracedecay_domain::GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()>;

    /// Clear one active repository quarantine only with a fresh, native
    /// recovery proof. Implementations retain the proof durably so a later
    /// reader can distinguish a proven clear from an accidental deletion.
    fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &tracedecay_domain::GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()>;
}
