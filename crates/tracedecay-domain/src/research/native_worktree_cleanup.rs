//! Durable native-worktree cleanup transaction contracts.
//!
//! Cleanup is part of the canonical native-integration transaction authority.
//! The immutable command names only daemon-resolved roots and deliberately has
//! no force or branch-deletion option.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    DomainError, ManifestDigest, ProjectId, RepositoryId, ScopeSetId, ScopeSetRevision, UtcMicros,
    WorktreeId, canonical_sha256,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorktreeCleanupCommandV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub repository_root: PathBuf,
    pub worktree_root: PathBuf,
}

impl NativeWorktreeCleanupCommandV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        if !self.repository_root.is_absolute()
            || !self.worktree_root.is_absolute()
            || self.repository_root == self.worktree_root
        {
            return Err(DomainError::NonCanonical {
                field: "native worktree cleanup roots",
            });
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeWorktreeCleanupPhaseV1 {
    Prepared,
    MutationStarted,
    NeedsReconciliation,
    Terminal,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeWorktreeCleanupOutcomeV1 {
    Removed,
    AbortedNoChange,
    RefusedForeignDrift,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorktreeCleanupTransactionV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub inspection_digest: ManifestDigest,
    pub confirmed_at: UtcMicros,
    pub confirmation_digest: ManifestDigest,
    pub command: NativeWorktreeCleanupCommandV1,
    pub phase: NativeWorktreeCleanupPhaseV1,
    pub phase_revision: u64,
    pub prepared_at: UtcMicros,
    pub updated_at: UtcMicros,
    pub terminal_outcome: Option<NativeWorktreeCleanupOutcomeV1>,
    pub transaction_digest: ManifestDigest,
}

impl NativeWorktreeCleanupTransactionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope_set_id.validate()?;
        self.scope_set_revision.validate()?;
        self.scope_set_digest.validate()?;
        self.inspection_digest.validate()?;
        self.confirmation_digest.validate()?;
        self.command.validate()?;
        self.transaction_digest.validate()?;
        if self.phase_revision == 0
            || self.confirmed_at.0 > self.prepared_at.0
            || self.updated_at.0 < self.prepared_at.0
            || (self.phase == NativeWorktreeCleanupPhaseV1::Terminal)
                != self.terminal_outcome.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "native worktree cleanup transaction state",
            });
        }
        let mut unsigned = self.clone();
        unsigned.transaction_digest = zero_digest()?;
        if canonical_sha256(&unsigned)? != self.transaction_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.transaction_digest = zero_digest()?;
        self.transaction_digest = canonical_sha256(&self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn same_intent(&self, other: &Self) -> bool {
        self.scope_set_id == other.scope_set_id
            && self.scope_set_revision == other.scope_set_revision
            && self.scope_set_digest == other.scope_set_digest
            && self.inspection_digest == other.inspection_digest
            && self.confirmed_at == other.confirmed_at
            && self.confirmation_digest == other.confirmation_digest
            && self.command == other.command
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.same_intent(other) && self.prepared_at == other.prepared_at
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorktreeCleanupReceiptV1 {
    pub transaction: NativeWorktreeCleanupTransactionV1,
    pub completed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl NativeWorktreeCleanupReceiptV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.transaction.validate()?;
        self.receipt_digest.validate()?;
        if self.transaction.phase != NativeWorktreeCleanupPhaseV1::Terminal
            || self.completed_at != self.transaction.updated_at
        {
            return Err(DomainError::NonCanonical {
                field: "native worktree cleanup terminal receipt",
            });
        }
        let mut unsigned = self.clone();
        unsigned.receipt_digest = zero_digest()?;
        if canonical_sha256(&unsigned)? != self.receipt_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.receipt_digest = zero_digest()?;
        self.receipt_digest = canonical_sha256(&self)?;
        self.validate()?;
        Ok(self)
    }
}

fn zero_digest() -> Result<ManifestDigest, DomainError> {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
}
