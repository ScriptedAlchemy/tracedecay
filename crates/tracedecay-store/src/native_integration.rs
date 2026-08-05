//! Persistence contract for daemon-owned native branch integration.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DomainError, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationIdempotencyKey,
    NativeIntegrationJournalPhaseV1, NativeIntegrationJournalV1, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationRecoveryReceiptV1, NativeIntegrationTransactionId,
    RepositoryId,
};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum NativeIntegrationStoreError {
    #[error("native integration preview conflicts with another immutable preview")]
    PreviewConflict,
    #[error("native integration approval was already consumed")]
    ApprovalConsumed,
    #[error("native integration idempotency key conflicts with another input")]
    IdempotencyConflict,
    #[error("native integration journal compare-and-swap failed")]
    JournalConflict,
    #[error("native integration receipt conflicts with another terminal result")]
    ReceiptConflict,
    #[error("repository is quarantined pending native integration inspection")]
    RepositoryQuarantined,
    #[error("native integration store is unavailable")]
    Unavailable,
    #[error("native integration store data is invalid: {0}")]
    InvalidData(String),
}

impl From<DomainError> for NativeIntegrationStoreError {
    fn from(error: DomainError) -> Self {
        Self::InvalidData(error.to_string())
    }
}

pub type NativeIntegrationStoreResult<T> = Result<T, NativeIntegrationStoreError>;

/// Durable transaction record. Approval plaintext is never retained, but its
/// one-use identity and content digest are immutable and unique in storage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationRecordV1 {
    pub idempotency_key: NativeIntegrationIdempotencyKey,
    pub input_digest: ManifestDigest,
    pub approval_id: NativeIntegrationApprovalId,
    pub approval_digest: ManifestDigest,
    pub preview: NativeIntegrationPreviewV1,
    pub journal: NativeIntegrationJournalV1,
    pub terminal_receipt: Option<NativeIntegrationReceiptV1>,
}

impl NativeIntegrationRecordV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.idempotency_key.validate()?;
        self.input_digest.validate()?;
        self.approval_id.validate()?;
        self.approval_digest.validate()?;
        self.preview.validate()?;
        self.journal.validate()?;
        if self.journal.preview_id != self.preview.preview_id
            || self.journal.preview_digest != self.preview.preview_digest
            || self.journal.repository_id != self.preview.repository_id
            || self.journal.source_worktree_id != self.preview.source_worktree_id
            || self.journal.destination_worktree_id != self.preview.destination_worktree_id
            || self.journal.destination_checked_out != self.preview.destination_checked_out
            || self.journal.mode != self.preview.mode
            || self.journal.source_tip != self.preview.source_tip
            || self.journal.expected_destination_tip != self.preview.destination_tip
            || self.journal.expected_destination_tree != self.preview.destination_tree
            || self.journal.expected_new_destination_tip != self.preview.candidate_destination_tip
            || self.journal.expected_repository_snapshot_digest
                != self.preview.repository_snapshot_digest
            || self.journal.candidate_tree != self.preview.candidate_tree
        {
            return Err(noncanonical("native integration record preview binding"));
        }
        match &self.terminal_receipt {
            None if self.journal.phase.is_terminal() => {
                Err(noncanonical("native integration terminal receipt"))
            }
            None => Ok(()),
            Some(_) if !self.journal.phase.is_terminal() => {
                Err(noncanonical("native integration premature receipt"))
            }
            Some(receipt) => {
                receipt.validate_against(&self.journal)?;
                if receipt.committed_at != self.journal.updated_at {
                    return Err(noncanonical("native integration receipt timing"));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationBeginRequestV1 {
    pub idempotency_key: NativeIntegrationIdempotencyKey,
    pub input_digest: ManifestDigest,
    pub approval_id: NativeIntegrationApprovalId,
    pub approval_digest: ManifestDigest,
    pub preview: NativeIntegrationPreviewV1,
    pub journal: NativeIntegrationJournalV1,
}

impl NativeIntegrationBeginRequestV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        NativeIntegrationRecordV1 {
            idempotency_key: self.idempotency_key.clone(),
            input_digest: self.input_digest.clone(),
            approval_id: self.approval_id.clone(),
            approval_digest: self.approval_digest.clone(),
            preview: self.preview.clone(),
            journal: self.journal.clone(),
            terminal_receipt: None,
        }
        .validate()?;
        if self.journal.phase != NativeIntegrationJournalPhaseV1::Prepared
            || self.journal.revision != 1
        {
            return Err(noncanonical("native integration begin journal"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum NativeIntegrationBeginResultV1 {
    Started(Box<NativeIntegrationRecordV1>),
    Replay(Box<NativeIntegrationReceiptV1>),
    RecoveryRequired(Box<NativeIntegrationRecordV1>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationTerminalWriteV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub expected_current_revision: u64,
    pub journal: NativeIntegrationJournalV1,
    pub receipt: NativeIntegrationReceiptV1,
}

impl NativeIntegrationTerminalWriteV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.transaction_id.validate()?;
        self.journal.validate()?;
        self.receipt.validate_against(&self.journal)?;
        let next_revision = self
            .expected_current_revision
            .checked_add(1)
            .ok_or_else(|| noncanonical("native integration terminal revision"))?;
        if self.expected_current_revision == 0
            || self.journal.revision != next_revision
            || self.transaction_id != self.journal.transaction_id
        {
            return Err(noncanonical("native integration terminal write"));
        }
        Ok(())
    }
}

pub trait NativeIntegrationStore {
    fn begin_or_replay(
        &self,
        request: NativeIntegrationBeginRequestV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1>;

    fn read_transaction(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>>;

    fn compare_and_swap_journal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_revision: u64,
        replacement: NativeIntegrationJournalV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationJournalV1>;

    fn write_terminal(
        &self,
        write: NativeIntegrationTerminalWriteV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1>;

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>>;

    fn recovery_repositories(&self) -> NativeIntegrationStoreResult<Vec<RepositoryId>>;

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()>;

    fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
        recovery_receipt: NativeIntegrationRecoveryReceiptV1,
    ) -> NativeIntegrationStoreResult<()>;
}

fn noncanonical(field: &'static str) -> DomainError {
    DomainError::NonCanonical { field }
}
