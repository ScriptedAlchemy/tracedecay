//! Durable contracts for exact native branch integration.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::time::UtcMicros;
use crate::research::{DomainError, ManifestDigest, RepositoryId, WorktreeId, canonical_sha256};

use super::{GitOidV1, validate_path_label};

/// The only branch-integration mechanics representable by the V2 native
/// mutation boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationMechanicalModeV1 {
    FastForward,
    TwoParentMerge,
    CherryPickExactCommits,
}

/// Durable state around native mutation boundaries.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationJournalPhaseV1 {
    Prepared,
    NativeApplyStarted,
    ObjectsWritten,
    DestinationMaterialized,
    RefCommitted,
    Verifying,
    Committed,
    AbortedNoChange,
    RolledBack,
    NeedsInspection,
}

impl NativeIntegrationJournalPhaseV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::AbortedNoChange | Self::RolledBack | Self::NeedsInspection
        )
    }
}

/// Reader-facing status is derived from the durable journal rather than a
/// process-local task registry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationStatusV1 {
    Queued,
    Running,
    Cancelling,
    CommitPointCrossed,
    Verifying,
    Committed,
    AbortedNoChange,
    RolledBack,
    NeedsInspection,
}

/// Durable apply, status, cancellation, and recovery authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationJournalV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub repository_id: RepositoryId,
    pub source_worktree_id: WorktreeId,
    pub destination_worktree_id: WorktreeId,
    pub destination_checked_out: bool,
    pub mode: NativeIntegrationMechanicalModeV1,
    pub source_tip: GitOidV1,
    pub expected_destination_tip: GitOidV1,
    pub expected_destination_tree: GitOidV1,
    pub expected_new_destination_tip: GitOidV1,
    pub expected_repository_snapshot_digest: ManifestDigest,
    pub candidate_tree: GitOidV1,
    pub phase: NativeIntegrationJournalPhaseV1,
    pub revision: u64,
    pub cancellation_requested_at: Option<UtcMicros>,
    pub ref_commit_observed: bool,
    pub started_at: UtcMicros,
    pub updated_at: UtcMicros,
}

impl NativeIntegrationJournalV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn prepared(
        transaction_id: NativeIntegrationTransactionId,
        preview_id: NativeIntegrationPreviewId,
        preview_digest: ManifestDigest,
        repository_id: RepositoryId,
        source_worktree_id: WorktreeId,
        destination_worktree_id: WorktreeId,
        mode: NativeIntegrationMechanicalModeV1,
        source_tip: GitOidV1,
        expected_destination_tip: GitOidV1,
        expected_destination_tree: GitOidV1,
        expected_new_destination_tip: GitOidV1,
        expected_repository_snapshot_digest: ManifestDigest,
        candidate_tree: GitOidV1,
        started_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let journal = Self {
            transaction_id,
            preview_id,
            preview_digest,
            repository_id,
            source_worktree_id,
            destination_worktree_id,
            destination_checked_out: false,
            mode,
            source_tip,
            expected_destination_tip,
            expected_destination_tree,
            expected_new_destination_tip,
            expected_repository_snapshot_digest,
            candidate_tree,
            phase: NativeIntegrationJournalPhaseV1::Prepared,
            revision: 1,
            cancellation_requested_at: None,
            ref_commit_observed: false,
            started_at,
            updated_at: started_at,
        };
        journal.validate()?;
        Ok(journal)
    }

    /// Set only while assembling the initial journal, before it is persisted.
    pub fn mark_destination_checked_out(&mut self) -> Result<(), DomainError> {
        if self.phase != NativeIntegrationJournalPhaseV1::Prepared
            || self.revision != 1
            || self.cancellation_requested_at.is_some()
        {
            return Err(noncanonical("native integration destination occupancy"));
        }
        self.destination_checked_out = true;
        self.validate()
    }

    pub fn advance(
        &mut self,
        successor: NativeIntegrationJournalPhaseV1,
        updated_at: UtcMicros,
    ) -> Result<(), DomainError> {
        if !self.permits_successor(successor) || updated_at < self.updated_at {
            return Err(noncanonical("native integration journal transition"));
        }
        self.phase = successor;
        self.ref_commit_observed |= successor == NativeIntegrationJournalPhaseV1::RefCommitted;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| noncanonical("native integration journal revision"))?;
        self.updated_at = updated_at;
        self.validate()
    }

    /// Persist an idempotent cancellation request. Native execution may honor
    /// it only before a destination materialization or ref commit boundary.
    pub fn request_cancellation(&mut self, requested_at: UtcMicros) -> Result<bool, DomainError> {
        if requested_at < self.updated_at {
            return Err(noncanonical("native integration cancellation timing"));
        }
        if self.phase.is_terminal() || self.cancellation_requested_at.is_some() {
            return Ok(false);
        }
        self.cancellation_requested_at = Some(requested_at);
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| noncanonical("native integration journal revision"))?;
        self.updated_at = requested_at;
        self.validate()?;
        Ok(true)
    }

    pub const fn commit_point_crossed(&self) -> bool {
        self.ref_commit_observed
    }

    pub const fn should_abort_before_commit(&self) -> bool {
        self.cancellation_requested_at.is_some()
            && matches!(
                self.phase,
                NativeIntegrationJournalPhaseV1::Prepared
                    | NativeIntegrationJournalPhaseV1::NativeApplyStarted
                    | NativeIntegrationJournalPhaseV1::ObjectsWritten
            )
    }

    pub const fn requires_recovery(&self) -> bool {
        !self.phase.is_terminal()
    }

    pub const fn status(&self) -> NativeIntegrationStatusV1 {
        match self.phase {
            NativeIntegrationJournalPhaseV1::Prepared
            | NativeIntegrationJournalPhaseV1::NativeApplyStarted
            | NativeIntegrationJournalPhaseV1::ObjectsWritten
                if self.cancellation_requested_at.is_some() =>
            {
                NativeIntegrationStatusV1::Cancelling
            }
            NativeIntegrationJournalPhaseV1::Prepared => NativeIntegrationStatusV1::Queued,
            NativeIntegrationJournalPhaseV1::NativeApplyStarted
            | NativeIntegrationJournalPhaseV1::ObjectsWritten
            | NativeIntegrationJournalPhaseV1::DestinationMaterialized => {
                NativeIntegrationStatusV1::Running
            }
            NativeIntegrationJournalPhaseV1::RefCommitted => {
                NativeIntegrationStatusV1::CommitPointCrossed
            }
            NativeIntegrationJournalPhaseV1::Verifying => NativeIntegrationStatusV1::Verifying,
            NativeIntegrationJournalPhaseV1::Committed => NativeIntegrationStatusV1::Committed,
            NativeIntegrationJournalPhaseV1::AbortedNoChange => {
                NativeIntegrationStatusV1::AbortedNoChange
            }
            NativeIntegrationJournalPhaseV1::RolledBack => NativeIntegrationStatusV1::RolledBack,
            NativeIntegrationJournalPhaseV1::NeedsInspection => {
                NativeIntegrationStatusV1::NeedsInspection
            }
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.transaction_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.repository_id.validate()?;
        self.source_worktree_id.validate()?;
        self.destination_worktree_id.validate()?;
        self.source_tip.validate()?;
        self.expected_destination_tip.validate()?;
        self.expected_destination_tree.validate()?;
        self.expected_new_destination_tip.validate()?;
        self.expected_repository_snapshot_digest.validate()?;
        self.candidate_tree.validate()?;
        if self.source_worktree_id == self.destination_worktree_id
            || self.expected_destination_tip == self.expected_new_destination_tip
            || self.revision == 0
            || self.updated_at < self.started_at
            || self
                .cancellation_requested_at
                .is_some_and(|requested_at| requested_at > self.updated_at)
            || (self.ref_commit_observed
                && matches!(
                    self.phase,
                    NativeIntegrationJournalPhaseV1::Prepared
                        | NativeIntegrationJournalPhaseV1::NativeApplyStarted
                        | NativeIntegrationJournalPhaseV1::ObjectsWritten
                        | NativeIntegrationJournalPhaseV1::DestinationMaterialized
                        | NativeIntegrationJournalPhaseV1::AbortedNoChange
                ))
        {
            return Err(noncanonical("native integration journal"));
        }
        Ok(())
    }

    fn permits_successor(&self, successor: NativeIntegrationJournalPhaseV1) -> bool {
        use NativeIntegrationJournalPhaseV1 as Phase;
        match (self.phase, successor) {
            (Phase::Prepared, Phase::NativeApplyStarted | Phase::AbortedNoChange) => true,
            (
                Phase::NativeApplyStarted,
                Phase::ObjectsWritten | Phase::AbortedNoChange | Phase::NeedsInspection,
            ) => true,
            (
                Phase::ObjectsWritten,
                Phase::AbortedNoChange
                | Phase::DestinationMaterialized
                | Phase::RefCommitted
                | Phase::NeedsInspection,
            ) => {
                (successor == Phase::DestinationMaterialized && self.destination_checked_out)
                    || (successor == Phase::RefCommitted && !self.destination_checked_out)
                    || matches!(successor, Phase::AbortedNoChange | Phase::NeedsInspection)
            }
            (
                Phase::DestinationMaterialized,
                Phase::RefCommitted | Phase::RolledBack | Phase::NeedsInspection,
            ) => true,
            (
                Phase::RefCommitted,
                Phase::Verifying | Phase::RolledBack | Phase::NeedsInspection,
            ) => true,
            (Phase::Verifying, Phase::Committed | Phase::RolledBack | Phase::NeedsInspection) => {
                true
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationReceiptOutcomeV1 {
    Committed,
    AbortedNoChange,
    RolledBack,
    NeedsInspection,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationReceiptV1 {
    pub receipt_id: NativeIntegrationReceiptId,
    pub transaction_id: NativeIntegrationTransactionId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub repository_id: RepositoryId,
    pub mode: NativeIntegrationMechanicalModeV1,
    pub old_destination_tip: GitOidV1,
    pub old_destination_tree: GitOidV1,
    pub expected_new_destination_tip: GitOidV1,
    pub candidate_tree: GitOidV1,
    pub final_snapshot_digest: Option<ManifestDigest>,
    pub final_destination_tip: Option<GitOidV1>,
    pub final_destination_tree: Option<GitOidV1>,
    pub created_commits: Vec<GitOidV1>,
    pub outcome: NativeIntegrationReceiptOutcomeV1,
    pub committed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

#[derive(Serialize)]
struct NativeIntegrationReceiptDigestMaterial<'a> {
    domain: &'static str,
    receipt_id: &'a NativeIntegrationReceiptId,
    transaction_id: &'a NativeIntegrationTransactionId,
    preview_id: &'a NativeIntegrationPreviewId,
    preview_digest: &'a ManifestDigest,
    repository_id: &'a RepositoryId,
    mode: NativeIntegrationMechanicalModeV1,
    old_destination_tip: &'a GitOidV1,
    old_destination_tree: &'a GitOidV1,
    expected_new_destination_tip: &'a GitOidV1,
    candidate_tree: &'a GitOidV1,
    final_snapshot_digest: Option<&'a ManifestDigest>,
    final_destination_tip: Option<&'a GitOidV1>,
    final_destination_tree: Option<&'a GitOidV1>,
    created_commits: &'a [GitOidV1],
    outcome: NativeIntegrationReceiptOutcomeV1,
    committed_at: UtcMicros,
}

impl NativeIntegrationReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        receipt_id: NativeIntegrationReceiptId,
        journal: &NativeIntegrationJournalV1,
        final_snapshot_digest: Option<ManifestDigest>,
        final_destination_tip: Option<GitOidV1>,
        final_destination_tree: Option<GitOidV1>,
        created_commits: Vec<GitOidV1>,
        outcome: NativeIntegrationReceiptOutcomeV1,
        committed_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        journal.validate()?;
        let mut receipt = Self {
            receipt_id,
            transaction_id: journal.transaction_id.clone(),
            preview_id: journal.preview_id.clone(),
            preview_digest: journal.preview_digest.clone(),
            repository_id: journal.repository_id.clone(),
            mode: journal.mode,
            old_destination_tip: journal.expected_destination_tip.clone(),
            old_destination_tree: journal.expected_destination_tree.clone(),
            expected_new_destination_tip: journal.expected_new_destination_tip.clone(),
            candidate_tree: journal.candidate_tree.clone(),
            final_snapshot_digest,
            final_destination_tip,
            final_destination_tree,
            created_commits,
            outcome,
            committed_at,
            receipt_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
        };
        receipt.receipt_digest = receipt.compute_receipt_digest()?;
        receipt.validate_against(journal)?;
        Ok(receipt)
    }

    pub fn compute_receipt_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        canonical_sha256(&NativeIntegrationReceiptDigestMaterial {
            domain: "tracedecay.native-integration.receipt.v1",
            receipt_id: &self.receipt_id,
            transaction_id: &self.transaction_id,
            preview_id: &self.preview_id,
            preview_digest: &self.preview_digest,
            repository_id: &self.repository_id,
            mode: self.mode,
            old_destination_tip: &self.old_destination_tip,
            old_destination_tree: &self.old_destination_tree,
            expected_new_destination_tip: &self.expected_new_destination_tip,
            candidate_tree: &self.candidate_tree,
            final_snapshot_digest: self.final_snapshot_digest.as_ref(),
            final_destination_tip: self.final_destination_tip.as_ref(),
            final_destination_tree: self.final_destination_tree.as_ref(),
            created_commits: &self.created_commits,
            outcome: self.outcome,
            committed_at: self.committed_at,
        })
    }

    pub fn validate_against(
        &self,
        journal: &NativeIntegrationJournalV1,
    ) -> Result<(), DomainError> {
        journal.validate()?;
        self.validate_fields()?;
        if self.receipt_digest != self.compute_receipt_digest()?
            || self.transaction_id != journal.transaction_id
            || self.preview_id != journal.preview_id
            || self.preview_digest != journal.preview_digest
            || self.repository_id != journal.repository_id
            || self.mode != journal.mode
            || self.old_destination_tip != journal.expected_destination_tip
            || self.old_destination_tree != journal.expected_destination_tree
            || self.expected_new_destination_tip != journal.expected_new_destination_tip
            || self.candidate_tree != journal.candidate_tree
            || self.outcome.phase() != journal.phase
        {
            return Err(noncanonical("native integration receipt binding"));
        }
        match self.outcome {
            NativeIntegrationReceiptOutcomeV1::Committed => {
                if self.final_snapshot_digest.is_none()
                    || self.final_destination_tip.as_ref()
                        != Some(&journal.expected_new_destination_tip)
                    || self.final_destination_tree.as_ref() != Some(&journal.candidate_tree)
                {
                    return Err(noncanonical("committed native integration receipt"));
                }
            }
            NativeIntegrationReceiptOutcomeV1::AbortedNoChange
            | NativeIntegrationReceiptOutcomeV1::RolledBack => {
                if self.final_snapshot_digest.as_ref()
                    != Some(&journal.expected_repository_snapshot_digest)
                    || self.final_destination_tip.as_ref()
                        != Some(&journal.expected_destination_tip)
                    || self.final_destination_tree.as_ref()
                        != Some(&journal.expected_destination_tree)
                    || (self.outcome == NativeIntegrationReceiptOutcomeV1::AbortedNoChange
                        && !self.created_commits.is_empty())
                {
                    return Err(noncanonical("unchanged native integration receipt"));
                }
            }
            NativeIntegrationReceiptOutcomeV1::NeedsInspection => {}
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.transaction_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.repository_id.validate()?;
        self.old_destination_tip.validate()?;
        self.old_destination_tree.validate()?;
        self.expected_new_destination_tip.validate()?;
        self.candidate_tree.validate()?;
        if let Some(digest) = &self.final_snapshot_digest {
            digest.validate()?;
        }
        if let Some(tip) = &self.final_destination_tip {
            tip.validate()?;
        }
        if let Some(tree) = &self.final_destination_tree {
            tree.validate()?;
        }
        for commit in &self.created_commits {
            commit.validate()?;
        }
        let final_capture_fields = [
            self.final_snapshot_digest.is_some(),
            self.final_destination_tip.is_some(),
            self.final_destination_tree.is_some(),
        ];
        if final_capture_fields.iter().any(|present| *present)
            && !final_capture_fields.iter().all(|present| *present)
        {
            return Err(noncanonical("native integration final snapshot"));
        }
        match self.mode {
            NativeIntegrationMechanicalModeV1::FastForward if !self.created_commits.is_empty() => {
                return Err(noncanonical("fast-forward created commits"));
            }
            NativeIntegrationMechanicalModeV1::TwoParentMerge
                if self.created_commits.len() != 1
                    || self.created_commits.first() != Some(&self.expected_new_destination_tip) =>
            {
                return Err(noncanonical("merge created commit"));
            }
            NativeIntegrationMechanicalModeV1::CherryPickExactCommits
                if self.created_commits.is_empty()
                    || self.created_commits.last() != Some(&self.expected_new_destination_tip) =>
            {
                return Err(noncanonical("cherry-pick created commits"));
            }
            _ => {}
        }
        Ok(())
    }
}

impl NativeIntegrationReceiptOutcomeV1 {
    const fn phase(self) -> NativeIntegrationJournalPhaseV1 {
        match self {
            Self::Committed => NativeIntegrationJournalPhaseV1::Committed,
            Self::AbortedNoChange => NativeIntegrationJournalPhaseV1::AbortedNoChange,
            Self::RolledBack => NativeIntegrationJournalPhaseV1::RolledBack,
            Self::NeedsInspection => NativeIntegrationJournalPhaseV1::NeedsInspection,
        }
    }
}

fn noncanonical(field: &'static str) -> DomainError {
    DomainError::NonCanonical { field }
}

crate::canonical_text::validated_string_newtype!(
    plain,
    DomainError,
    validate_path_label;
    NativeIntegrationPreviewId => "native integration preview id",
    NativeIntegrationTransactionId => "native integration transaction id",
    NativeIntegrationReceiptId => "native integration receipt id",
    NativeIntegrationApprovalId => "native integration approval id",
    NativeIntegrationIdempotencyKey => "native integration idempotency key",
);
