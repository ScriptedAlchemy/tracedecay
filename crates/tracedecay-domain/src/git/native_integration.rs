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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationPreviewBlockerV1 {
    NativeConflict,
    SemanticConflict,
    PartialEvidence,
    UnsupportedRepositoryState,
    UnsupportedHooks,
    UnsupportedSigning,
    UnsupportedDriver,
    DependencyNotReady,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "blockers")]
pub enum NativeIntegrationPreviewDispositionV1 {
    Eligible,
    PreviewOnly(Vec<NativeIntegrationPreviewBlockerV1>),
}

impl NativeIntegrationPreviewDispositionV1 {
    pub const fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible)
    }
}

/// Immutable result of private native preflight.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationPreviewV1 {
    pub preview_id: NativeIntegrationPreviewId,
    pub repository_id: RepositoryId,
    pub source_worktree_id: WorktreeId,
    pub destination_worktree_id: WorktreeId,
    pub source_ref: String,
    pub destination_ref: String,
    pub destination_checked_out: bool,
    pub mode: NativeIntegrationMechanicalModeV1,
    pub source_tip: GitOidV1,
    pub destination_tip: GitOidV1,
    pub destination_tree: GitOidV1,
    pub merge_base: GitOidV1,
    pub ordered_source_commits: Vec<GitOidV1>,
    pub expected_created_commits: Vec<GitOidV1>,
    pub candidate_destination_tip: GitOidV1,
    pub candidate_tree: GitOidV1,
    pub repository_snapshot_digest: ManifestDigest,
    pub selection_digest: ManifestDigest,
    pub topology_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub attributes_digest: ManifestDigest,
    pub hook_policy_digest: ManifestDigest,
    pub signing_policy_digest: ManifestDigest,
    pub message_policy_digest: ManifestDigest,
    pub semantic_evidence_digest: ManifestDigest,
    pub disposition: NativeIntegrationPreviewDispositionV1,
    pub created_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub preview_digest: ManifestDigest,
}

#[derive(Serialize)]
struct NativeIntegrationPreviewDigestMaterial<'a> {
    domain: &'static str,
    preview_id: &'a NativeIntegrationPreviewId,
    repository_id: &'a RepositoryId,
    source_worktree_id: &'a WorktreeId,
    destination_worktree_id: &'a WorktreeId,
    source_ref: &'a str,
    destination_ref: &'a str,
    destination_checked_out: bool,
    mode: NativeIntegrationMechanicalModeV1,
    source_tip: &'a GitOidV1,
    destination_tip: &'a GitOidV1,
    destination_tree: &'a GitOidV1,
    merge_base: &'a GitOidV1,
    ordered_source_commits: &'a [GitOidV1],
    expected_created_commits: &'a [GitOidV1],
    candidate_destination_tip: &'a GitOidV1,
    candidate_tree: &'a GitOidV1,
    repository_snapshot_digest: &'a ManifestDigest,
    selection_digest: &'a ManifestDigest,
    topology_digest: &'a ManifestDigest,
    configuration_digest: &'a ManifestDigest,
    attributes_digest: &'a ManifestDigest,
    hook_policy_digest: &'a ManifestDigest,
    signing_policy_digest: &'a ManifestDigest,
    message_policy_digest: &'a ManifestDigest,
    semantic_evidence_digest: &'a ManifestDigest,
    disposition: &'a NativeIntegrationPreviewDispositionV1,
    created_at: UtcMicros,
    expires_at: UtcMicros,
}

impl NativeIntegrationPreviewV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.preview_digest = self.compute_preview_digest()?;
        self.validate()?;
        Ok(self)
    }

    pub fn compute_preview_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        canonical_sha256(&NativeIntegrationPreviewDigestMaterial {
            domain: "tracedecay.native-integration.preview.v1",
            preview_id: &self.preview_id,
            repository_id: &self.repository_id,
            source_worktree_id: &self.source_worktree_id,
            destination_worktree_id: &self.destination_worktree_id,
            source_ref: &self.source_ref,
            destination_ref: &self.destination_ref,
            destination_checked_out: self.destination_checked_out,
            mode: self.mode,
            source_tip: &self.source_tip,
            destination_tip: &self.destination_tip,
            destination_tree: &self.destination_tree,
            merge_base: &self.merge_base,
            ordered_source_commits: &self.ordered_source_commits,
            expected_created_commits: &self.expected_created_commits,
            candidate_destination_tip: &self.candidate_destination_tip,
            candidate_tree: &self.candidate_tree,
            repository_snapshot_digest: &self.repository_snapshot_digest,
            selection_digest: &self.selection_digest,
            topology_digest: &self.topology_digest,
            configuration_digest: &self.configuration_digest,
            attributes_digest: &self.attributes_digest,
            hook_policy_digest: &self.hook_policy_digest,
            signing_policy_digest: &self.signing_policy_digest,
            message_policy_digest: &self.message_policy_digest,
            semantic_evidence_digest: &self.semantic_evidence_digest,
            disposition: &self.disposition,
            created_at: self.created_at,
            expires_at: self.expires_at,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.preview_digest.validate()?;
        self.validate_fields()?;
        if self.preview_digest != self.compute_preview_digest()? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.preview_id.validate()?;
        self.repository_id.validate()?;
        self.source_worktree_id.validate()?;
        self.destination_worktree_id.validate()?;
        validate_full_branch_ref(&self.source_ref)?;
        validate_full_branch_ref(&self.destination_ref)?;
        self.source_tip.validate()?;
        self.destination_tip.validate()?;
        self.destination_tree.validate()?;
        self.merge_base.validate()?;
        self.candidate_destination_tip.validate()?;
        self.candidate_tree.validate()?;
        for commit in self
            .ordered_source_commits
            .iter()
            .chain(&self.expected_created_commits)
        {
            commit.validate()?;
        }
        for digest in [
            &self.repository_snapshot_digest,
            &self.selection_digest,
            &self.topology_digest,
            &self.configuration_digest,
            &self.attributes_digest,
            &self.hook_policy_digest,
            &self.signing_policy_digest,
            &self.message_policy_digest,
            &self.semantic_evidence_digest,
        ] {
            digest.validate()?;
        }
        if self.source_worktree_id == self.destination_worktree_id
            || self.source_ref == self.destination_ref
            || self.source_tip == self.destination_tip
            || self.destination_tip == self.candidate_destination_tip
            || self.created_at >= self.expires_at
            || self.ordered_source_commits.is_empty()
            || has_duplicates(&self.ordered_source_commits)
            || has_duplicates(&self.expected_created_commits)
        {
            return Err(noncanonical("native integration preview"));
        }
        if let NativeIntegrationPreviewDispositionV1::PreviewOnly(blockers) = &self.disposition
            && (blockers.is_empty() || blockers.windows(2).any(|pair| pair[0] >= pair[1]))
        {
            return Err(noncanonical("native integration preview blockers"));
        }
        match self.mode {
            NativeIntegrationMechanicalModeV1::FastForward
                if self.candidate_destination_tip != self.source_tip
                    || !self.expected_created_commits.is_empty() => {}
            NativeIntegrationMechanicalModeV1::TwoParentMerge
                if self.expected_created_commits.len() != 1
                    || self.expected_created_commits.last()
                        != Some(&self.candidate_destination_tip) => {}
            NativeIntegrationMechanicalModeV1::CherryPickExactCommits
                if self.expected_created_commits.len() != self.ordered_source_commits.len()
                    || self.expected_created_commits.last()
                        != Some(&self.candidate_destination_tip) => {}
            NativeIntegrationMechanicalModeV1::FastForward
            | NativeIntegrationMechanicalModeV1::TwoParentMerge
            | NativeIntegrationMechanicalModeV1::CherryPickExactCommits => return Ok(()),
        }
        Err(noncanonical("native integration mechanical preview"))
    }
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

    pub fn prepared_from_preview(
        transaction_id: NativeIntegrationTransactionId,
        preview: &NativeIntegrationPreviewV1,
        started_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        preview.validate()?;
        if !preview.disposition.is_eligible() || started_at > preview.expires_at {
            return Err(noncanonical("native integration applicable preview"));
        }
        let mut journal = Self::prepared(
            transaction_id,
            preview.preview_id.clone(),
            preview.preview_digest.clone(),
            preview.repository_id.clone(),
            preview.source_worktree_id.clone(),
            preview.destination_worktree_id.clone(),
            preview.mode,
            preview.source_tip.clone(),
            preview.destination_tip.clone(),
            preview.destination_tree.clone(),
            preview.candidate_destination_tip.clone(),
            preview.repository_snapshot_digest.clone(),
            preview.candidate_tree.clone(),
            started_at,
        )?;
        if preview.destination_checked_out {
            journal.mark_destination_checked_out()?;
        }
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

    /// Accept exactly one legal phase transition or one cancellation request.
    /// Stores use this before their compare-and-swap update so a caller cannot
    /// rewrite immutable bindings while presenting a plausible revision.
    pub fn permits_replacement(&self, replacement: &Self) -> bool {
        if self.transaction_id != replacement.transaction_id
            || self.preview_id != replacement.preview_id
            || self.preview_digest != replacement.preview_digest
            || self.repository_id != replacement.repository_id
            || self.source_worktree_id != replacement.source_worktree_id
            || self.destination_worktree_id != replacement.destination_worktree_id
            || self.destination_checked_out != replacement.destination_checked_out
            || self.mode != replacement.mode
            || self.source_tip != replacement.source_tip
            || self.expected_destination_tip != replacement.expected_destination_tip
            || self.expected_destination_tree != replacement.expected_destination_tree
            || self.expected_new_destination_tip != replacement.expected_new_destination_tip
            || self.expected_repository_snapshot_digest
                != replacement.expected_repository_snapshot_digest
            || self.candidate_tree != replacement.candidate_tree
            || self.started_at != replacement.started_at
        {
            return false;
        }

        let mut expected = self.clone();
        let transition = if self.phase != replacement.phase
            && self.cancellation_requested_at == replacement.cancellation_requested_at
        {
            expected.advance(replacement.phase, replacement.updated_at)
        } else if self.phase == replacement.phase
            && self.cancellation_requested_at.is_none()
            && replacement.cancellation_requested_at.is_some()
        {
            expected
                .request_cancellation(replacement.updated_at)
                .and_then(|changed| {
                    changed
                        .then_some(())
                        .ok_or_else(|| noncanonical("native integration cancellation replacement"))
                })
        } else {
            return false;
        };
        transition.is_ok() && expected == *replacement
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

fn validate_full_branch_ref(reference: &str) -> Result<(), DomainError> {
    validate_path_label(reference, "native integration branch ref")?;
    if !reference.starts_with("refs/heads/")
        || reference.ends_with('/')
        || reference.contains("..")
        || reference.contains("@{")
        || reference.contains('\\')
    {
        return Err(noncanonical("native integration branch ref"));
    }
    Ok(())
}

fn has_duplicates(values: &[GitOidV1]) -> bool {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.len() != values.len()
}

crate::canonical_text::validated_string_newtype!(
    plain,
    DomainError,
    validate_path_label;
    NativeIntegrationPreviewId => "native integration preview id",
    NativeIntegrationTransactionId => "native integration transaction id",
    NativeIntegrationReceiptId => "native integration receipt id",
    NativeIntegrationRecoveryReceiptId => "native integration recovery receipt id",
    NativeIntegrationApprovalId => "native integration approval id",
    NativeIntegrationPrincipalId => "native integration principal id",
    NativeIntegrationDelegatedAgentId => "native integration delegated agent id",
    NativeIntegrationIdempotencyKey => "native integration idempotency key",
);
