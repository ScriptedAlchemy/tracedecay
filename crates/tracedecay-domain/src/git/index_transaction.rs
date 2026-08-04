//! Durable Git index transaction journal and receipt contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::research::time::UtcMicros;
use crate::research::{DomainError, ManifestDigest, RepositoryId, WorktreeId, canonical_sha256};

use super::*;

/// Durable transaction phases. Recovery may reconcile a transaction only to
/// one of the terminal truth states; it never re-enters `NativeApplyStarted`
/// after a crash.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexJournalPhaseV1 {
    Prepared,
    NativeApplyStarted,
    IndexCommitted,
    RefCommitted,
    Verifying,
    Committed,
    AbortedNoChange,
    NeedsInspection,
}

impl GitIndexJournalPhaseV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::AbortedNoChange | Self::NeedsInspection
        )
    }

    pub const fn permits_successor(self, successor: Self) -> bool {
        matches!(
            (self, successor),
            (Self::Prepared, Self::NativeApplyStarted)
                | (Self::Prepared, Self::AbortedNoChange)
                | (Self::Prepared, Self::NeedsInspection)
                | (Self::NativeApplyStarted, Self::IndexCommitted)
                | (Self::NativeApplyStarted, Self::AbortedNoChange)
                | (Self::NativeApplyStarted, Self::NeedsInspection)
                | (Self::IndexCommitted, Self::RefCommitted)
                | (Self::IndexCommitted, Self::Verifying)
                | (Self::IndexCommitted, Self::NeedsInspection)
                | (Self::RefCommitted, Self::Verifying)
                | (Self::RefCommitted, Self::NeedsInspection)
                | (Self::Verifying, Self::Committed)
                | (Self::Verifying, Self::NeedsInspection)
        )
    }

    /// Whether a restart-only reconciliation can prove `outcome` from this
    /// durable phase. This deliberately requires evidence written *after* a
    /// native commit boundary: matching a candidate tree alone is not proof
    /// that this transaction published it.
    pub const fn permits_recovered_outcome(
        self,
        operation: GitIndexTransactionOperationV1,
        outcome: GitIndexReceiptOutcomeV1,
    ) -> bool {
        match outcome {
            GitIndexReceiptOutcomeV1::AbortedNoChange => {
                matches!(self, Self::Prepared | Self::NativeApplyStarted)
            }
            GitIndexReceiptOutcomeV1::NeedsInspection => !self.is_terminal(),
            GitIndexReceiptOutcomeV1::Committed => match operation {
                GitIndexTransactionOperationV1::StageHunks
                | GitIndexTransactionOperationV1::UnstageHunks => {
                    matches!(self, Self::IndexCommitted | Self::Verifying)
                }
                GitIndexTransactionOperationV1::CommitIndex => {
                    matches!(self, Self::RefCommitted | Self::Verifying)
                }
            },
        }
    }
}

/// Durable recovery record. The daemon fsyncs this record before the first
/// native mutation and after every legal phase transition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitIndexTransactionJournalV1 {
    pub transaction_id: GitIndexTransactionId,
    pub preview_id: GitIndexPreviewId,
    pub preview_digest: ManifestDigest,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub operation: GitIndexTransactionOperationV1,
    pub expected_snapshot_digest: ManifestDigest,
    pub phase: GitIndexJournalPhaseV1,
    pub phase_epoch: u64,
    pub started_at: UtcMicros,
    pub updated_at: UtcMicros,
}

impl GitIndexTransactionJournalV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn prepared(
        transaction_id: GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        started_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        preview.validate()?;
        let worktree_id =
            preview
                .repository_snapshot
                .worktree_id
                .clone()
                .ok_or(DomainError::NonCanonical {
                    field: "git index transaction worktree",
                })?;
        let journal = Self {
            transaction_id,
            preview_id: preview.preview_id.clone(),
            preview_digest: preview.preview_digest.clone(),
            repository_id: preview.repository_snapshot.repository_id.clone(),
            worktree_id,
            operation: preview.operation,
            expected_snapshot_digest: preview.repository_snapshot_digest.clone(),
            phase: GitIndexJournalPhaseV1::Prepared,
            phase_epoch: 1,
            started_at,
            updated_at: started_at,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn advance(
        &mut self,
        successor: GitIndexJournalPhaseV1,
        updated_at: UtcMicros,
    ) -> Result<(), DomainError> {
        if !self.phase.permits_successor(successor)
            || (successor == GitIndexJournalPhaseV1::RefCommitted
                && self.operation != GitIndexTransactionOperationV1::CommitIndex)
            || updated_at < self.updated_at
        {
            return Err(DomainError::NonCanonical {
                field: "git index transaction journal transition",
            });
        }
        self.phase = successor;
        self.phase_epoch = self
            .phase_epoch
            .checked_add(1)
            .ok_or(DomainError::NonCanonical {
                field: "git index transaction phase epoch",
            })?;
        self.updated_at = updated_at;
        self.validate()
    }

    pub fn requires_recovery(&self) -> bool {
        !self.phase.is_terminal()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.transaction_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        self.expected_snapshot_digest.validate()?;
        if self.updated_at < self.started_at || !self.has_canonical_phase_epoch() {
            return Err(DomainError::NonCanonical {
                field: "git index transaction journal timing",
            });
        }
        if self.operation != GitIndexTransactionOperationV1::CommitIndex
            && self.phase == GitIndexJournalPhaseV1::RefCommitted
        {
            return Err(DomainError::NonCanonical {
                field: "git index transaction ref commit phase",
            });
        }
        Ok(())
    }

    fn has_canonical_phase_epoch(&self) -> bool {
        let is_commit = self.operation == GitIndexTransactionOperationV1::CommitIndex;
        match self.phase {
            GitIndexJournalPhaseV1::Prepared => self.phase_epoch == 1,
            GitIndexJournalPhaseV1::NativeApplyStarted => self.phase_epoch == 2,
            GitIndexJournalPhaseV1::IndexCommitted => self.phase_epoch == 3,
            GitIndexJournalPhaseV1::RefCommitted => is_commit && self.phase_epoch == 4,
            GitIndexJournalPhaseV1::Verifying => self.phase_epoch == if is_commit { 5 } else { 4 },
            GitIndexJournalPhaseV1::Committed => self.phase_epoch == if is_commit { 6 } else { 5 },
            GitIndexJournalPhaseV1::AbortedNoChange => matches!(self.phase_epoch, 2 | 3),
            GitIndexJournalPhaseV1::NeedsInspection => {
                (2..=if is_commit { 6 } else { 5 }).contains(&self.phase_epoch)
            }
        }
    }
}

/// Terminal outcome a recovery record can prove without re-running a native
/// mutation.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexReceiptOutcomeV1 {
    Committed,
    AbortedNoChange,
    NeedsInspection,
}

/// Durable, integrity-protected receipt for one PR11 index transaction.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitIndexTransactionReceiptV1 {
    pub receipt_id: GitIndexReceiptId,
    pub transaction_id: GitIndexTransactionId,
    pub preview_id: GitIndexPreviewId,
    pub operation: GitIndexTransactionOperationV1,
    pub old_snapshot_digest: ManifestDigest,
    /// Digest of a snapshot that was actually captured after the terminal
    /// observation. When `final_snapshot_captured` is false this retains the
    /// expected snapshot digest only as a stable schema placeholder; callers
    /// must not treat it as an observation.
    pub final_snapshot_digest: ManifestDigest,
    /// Whether `final_snapshot_digest`, `new_index_tree`, and `new_head` came
    /// from a post-outcome native observation. An unavailable observation is
    /// valid only for a terminal outcome that does not claim a commit.
    pub final_snapshot_captured: bool,
    pub old_index_tree: Option<GitOidV1>,
    pub new_index_tree: Option<GitOidV1>,
    pub old_head: Option<GitOidV1>,
    pub new_head: Option<GitOidV1>,
    pub selected_hunk_digests: Vec<ManifestDigest>,
    pub created_commit: Option<GitOidV1>,
    pub outcome: GitIndexReceiptOutcomeV1,
    pub committed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

#[derive(Serialize)]
struct GitIndexReceiptDigestMaterial<'a> {
    domain: &'static str,
    receipt_id: &'a GitIndexReceiptId,
    transaction_id: &'a GitIndexTransactionId,
    preview_id: &'a GitIndexPreviewId,
    operation: GitIndexTransactionOperationV1,
    old_snapshot_digest: &'a ManifestDigest,
    final_snapshot_digest: &'a ManifestDigest,
    final_snapshot_captured: bool,
    old_index_tree: Option<&'a GitOidV1>,
    new_index_tree: Option<&'a GitOidV1>,
    old_head: Option<&'a GitOidV1>,
    new_head: Option<&'a GitOidV1>,
    selected_hunk_digests: &'a [ManifestDigest],
    created_commit: Option<&'a GitOidV1>,
    outcome: GitIndexReceiptOutcomeV1,
    committed_at: UtcMicros,
}

impl GitIndexTransactionReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receipt_id: GitIndexReceiptId,
        transaction_id: GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        final_snapshot_digest: ManifestDigest,
        new_index_tree: Option<GitOidV1>,
        new_head: Option<GitOidV1>,
        created_commit: Option<GitOidV1>,
        outcome: GitIndexReceiptOutcomeV1,
        committed_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        Self::new_with_final_snapshot(
            receipt_id,
            transaction_id,
            preview,
            Some(final_snapshot_digest),
            new_index_tree,
            new_head,
            created_commit,
            outcome,
            committed_at,
        )
    }

    /// Construct a receipt while representing an unavailable terminal native
    /// snapshot explicitly. The unavailable form never fabricates observed
    /// repository state and cannot be used for a committed outcome.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_final_snapshot(
        receipt_id: GitIndexReceiptId,
        transaction_id: GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        final_snapshot_digest: Option<ManifestDigest>,
        new_index_tree: Option<GitOidV1>,
        new_head: Option<GitOidV1>,
        created_commit: Option<GitOidV1>,
        outcome: GitIndexReceiptOutcomeV1,
        committed_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        preview.validate()?;
        let old_index_tree = preview.repository_snapshot.index.tree_id.clone();
        let old_head = preview.repository_snapshot.head.commit().cloned();
        let final_snapshot_captured = final_snapshot_digest.is_some();
        let mut receipt = Self {
            receipt_id,
            transaction_id,
            preview_id: preview.preview_id.clone(),
            operation: preview.operation,
            old_snapshot_digest: preview.repository_snapshot_digest.clone(),
            final_snapshot_digest: final_snapshot_digest
                .unwrap_or_else(|| preview.repository_snapshot_digest.clone()),
            final_snapshot_captured,
            old_index_tree,
            new_index_tree,
            old_head,
            new_head,
            selected_hunk_digests: preview.selected_hunk_digests()?,
            created_commit,
            outcome,
            committed_at,
            receipt_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
        };
        receipt.receipt_digest = receipt.compute_receipt_digest()?;
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn compute_receipt_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        canonical_sha256(&GitIndexReceiptDigestMaterial {
            domain: GIT_INDEX_RECEIPT_DIGEST_DOMAIN_V1,
            receipt_id: &self.receipt_id,
            transaction_id: &self.transaction_id,
            preview_id: &self.preview_id,
            operation: self.operation,
            old_snapshot_digest: &self.old_snapshot_digest,
            final_snapshot_digest: &self.final_snapshot_digest,
            final_snapshot_captured: self.final_snapshot_captured,
            old_index_tree: self.old_index_tree.as_ref(),
            new_index_tree: self.new_index_tree.as_ref(),
            old_head: self.old_head.as_ref(),
            new_head: self.new_head.as_ref(),
            selected_hunk_digests: &self.selected_hunk_digests,
            created_commit: self.created_commit.as_ref(),
            outcome: self.outcome,
            committed_at: self.committed_at,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.receipt_digest.validate()?;
        self.validate_fields()?;
        if self.receipt_digest != self.compute_receipt_digest()? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.transaction_id.validate()?;
        self.preview_id.validate()?;
        self.old_snapshot_digest.validate()?;
        self.final_snapshot_digest.validate()?;
        for digest in &self.selected_hunk_digests {
            digest.validate()?;
        }
        if self
            .selected_hunk_digests
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(DomainError::DuplicateId {
                field: "git index receipt hunk digest order",
            });
        }
        if self.operation.hunk_direction().is_some() && self.selected_hunk_digests.is_empty() {
            return Err(DomainError::Empty {
                field: "git index receipt hunk digests",
            });
        }
        if self.operation == GitIndexTransactionOperationV1::CommitIndex
            && !self.selected_hunk_digests.is_empty()
        {
            return Err(DomainError::NonCanonical {
                field: "git index commit receipt hunk digests",
            });
        }
        if self.operation != GitIndexTransactionOperationV1::CommitIndex
            && self.created_commit.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "git index hunk receipt created commit",
            });
        }
        if !self.final_snapshot_captured
            && (self.old_snapshot_digest != self.final_snapshot_digest
                || self.old_index_tree != self.new_index_tree
                || self.old_head != self.new_head
                || self.created_commit.is_some())
        {
            return Err(DomainError::SnapshotMismatch {
                field: "unobserved git index receipt placeholder state",
            });
        }
        if self.outcome == GitIndexReceiptOutcomeV1::Committed
            && (!self.final_snapshot_captured
                || self.new_index_tree.is_none()
                || (self.operation == GitIndexTransactionOperationV1::CommitIndex
                    && self.created_commit.is_none()))
        {
            return Err(DomainError::NonCanonical {
                field: "committed git index receipt outcome",
            });
        }
        if self.outcome == GitIndexReceiptOutcomeV1::AbortedNoChange
            && (self.old_snapshot_digest != self.final_snapshot_digest
                || self.old_index_tree != self.new_index_tree
                || self.old_head != self.new_head
                || self.created_commit.is_some())
        {
            return Err(DomainError::SnapshotMismatch {
                field: "aborted git index receipt state",
            });
        }
        Ok(())
    }
}

const fn default_true() -> bool {
    true
}

impl<'de> Deserialize<'de> for GitIndexTransactionReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            receipt_id: GitIndexReceiptId,
            transaction_id: GitIndexTransactionId,
            preview_id: GitIndexPreviewId,
            operation: GitIndexTransactionOperationV1,
            old_snapshot_digest: ManifestDigest,
            final_snapshot_digest: ManifestDigest,
            #[serde(default = "default_true")]
            final_snapshot_captured: bool,
            old_index_tree: Option<GitOidV1>,
            new_index_tree: Option<GitOidV1>,
            old_head: Option<GitOidV1>,
            new_head: Option<GitOidV1>,
            selected_hunk_digests: Vec<ManifestDigest>,
            created_commit: Option<GitOidV1>,
            outcome: GitIndexReceiptOutcomeV1,
            committed_at: UtcMicros,
            receipt_digest: ManifestDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        let receipt = Self {
            receipt_id: wire.receipt_id,
            transaction_id: wire.transaction_id,
            preview_id: wire.preview_id,
            operation: wire.operation,
            old_snapshot_digest: wire.old_snapshot_digest,
            final_snapshot_digest: wire.final_snapshot_digest,
            final_snapshot_captured: wire.final_snapshot_captured,
            old_index_tree: wire.old_index_tree,
            new_index_tree: wire.new_index_tree,
            old_head: wire.old_head,
            new_head: wire.new_head,
            selected_hunk_digests: wire.selected_hunk_digests,
            created_commit: wire.created_commit,
            outcome: wire.outcome,
            committed_at: wire.committed_at,
            receipt_digest: wire.receipt_digest,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}
