//! Pure classification of typed Git index effects.
//!
//! No generic command string, branch/tag/ref mutation, merge, rebase,
//! cherry-pick, push, or history rewrite is representable in this module.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, RepositoryStateSnapshotId, RepositoryStateSnapshotV1, UtcMicros,
};

use crate::authorization::{PolicyIdentifierV1, policy_digest};

/// The only index operations policy may classify. Native Git/application owns
/// previews, CAS guards, mutations, idempotency, and receipts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexEffectV1 {
    Preview,
    StageHunks,
    UnstageHunks,
    CommitIndex,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitEffectClassV1 {
    Preview,
    IndexMutation,
    CommitCreation,
}

impl GitIndexEffectV1 {
    pub const fn class(self) -> GitEffectClassV1 {
        match self {
            Self::Preview => GitEffectClassV1::Preview,
            Self::StageHunks | Self::UnstageHunks => GitEffectClassV1::IndexMutation,
            Self::CommitIndex => GitEffectClassV1::CommitCreation,
        }
    }

    const fn requires_preview(self) -> bool {
        !matches!(self, Self::Preview)
    }
}

/// Policy projection of the native repository snapshot. It carries no
/// filesystem path or Git handle and can be created from the domain snapshot
/// without opening a repository.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitRepositoryStateFactV1 {
    pub snapshot_id: RepositoryStateSnapshotId,
    pub snapshot_digest: ManifestDigest,
    pub mutation_eligible: bool,
}

impl GitRepositoryStateFactV1 {
    pub fn new(
        snapshot_id: impl Into<String>,
        snapshot_digest: ManifestDigest,
        mutation_eligible: bool,
    ) -> Result<Self, tracedecay_domain::DomainError> {
        Ok(Self {
            snapshot_id: RepositoryStateSnapshotId::new(snapshot_id)?,
            snapshot_digest,
            mutation_eligible,
        })
    }

    pub fn from_snapshot(snapshot: &RepositoryStateSnapshotV1) -> Self {
        Self {
            snapshot_id: snapshot.snapshot_id().clone(),
            snapshot_digest: policy_digest("tracedecay.policy.git-repository-state.v1", snapshot),
            mutation_eligible: snapshot.is_mutation_eligible(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitEffectAuthorizationV1 {
    pub capability_granted: bool,
    pub owner_scope_matches: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewPreconditionV1 {
    pub preview_digest: ManifestDigest,
    pub repository_state_id: RepositoryStateSnapshotId,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitConflictRiskV1 {
    NoneKnown,
    Possible,
    Confirmed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitEffectClassificationInputV1 {
    pub effect: GitIndexEffectV1,
    pub authorization: GitEffectAuthorizationV1,
    pub repository_state: GitRepositoryStateFactV1,
    /// Immutable digest carried by the proposed apply request.
    pub expected_preview_digest: Option<ManifestDigest>,
    pub preview: Option<GitPreviewPreconditionV1>,
    pub conflict_risk: GitConflictRiskV1,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub evaluated_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitEffectDispositionV1 {
    Allow,
    Deny,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitEffectReasonV1 {
    InvalidInput,
    CapabilityNotGranted,
    OwnerScopeMismatch,
    RepositoryStateIneligible,
    PreviewRequired,
    PreviewDigestMismatch,
    PreviewSnapshotMismatch,
    PossibleConflict,
    ConfirmedConflict,
    Classified,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitEffectDecisionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub effect_class: GitEffectClassV1,
    pub disposition: GitEffectDispositionV1,
    pub ordered_reason_codes: Vec<GitEffectReasonV1>,
}

pub trait GitEffectClassifier {
    fn evaluate(&self, input: &GitEffectClassificationInputV1) -> GitEffectDecisionV1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitEffectClassifierV1 {
    evaluator_id: PolicyIdentifierV1,
}

impl Default for GitEffectClassifierV1 {
    fn default() -> Self {
        Self {
            evaluator_id: PolicyIdentifierV1::new("git_effect_classification.v1")
                .expect("static evaluator identifier is valid"),
        }
    }
}

impl GitEffectClassifierV1 {
    /// Revision of this reviewed implementation, recorded with every decision
    /// so replay can refuse a substituted evaluator. It is a property of the
    /// code, not of an instance.
    const EVALUATOR_REVISION: u64 = 1;

    fn decision(
        &self,
        input: &GitEffectClassificationInputV1,
        disposition: GitEffectDispositionV1,
        ordered_reason_codes: Vec<GitEffectReasonV1>,
    ) -> GitEffectDecisionV1 {
        GitEffectDecisionV1 {
            evaluator_id: self.evaluator_id.clone(),
            evaluator_revision: Self::EVALUATOR_REVISION,
            input_digest: policy_digest("tracedecay.policy.git-effect-input.v1", input),
            policy_revision: input.policy_revision,
            policy_digest: input.policy_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            effect_class: input.effect.class(),
            disposition,
            ordered_reason_codes,
        }
    }
}

impl GitEffectClassifier for GitEffectClassifierV1 {
    fn evaluate(&self, input: &GitEffectClassificationInputV1) -> GitEffectDecisionV1 {
        if input.policy_revision == 0
            || input.policy_digest.validate().is_err()
            || input.configuration_digest.validate().is_err()
            || input.repository_state.snapshot_id.validate().is_err()
            || input.repository_state.snapshot_digest.validate().is_err()
            || input.preview.as_ref().is_some_and(|preview| {
                preview.preview_digest.validate().is_err()
                    || preview.repository_state_id.validate().is_err()
            })
            || input
                .expected_preview_digest
                .as_ref()
                .is_some_and(|digest| digest.validate().is_err())
        {
            return self.decision(
                input,
                GitEffectDispositionV1::Indeterminate,
                vec![GitEffectReasonV1::InvalidInput],
            );
        }
        if !input.authorization.capability_granted {
            return self.decision(
                input,
                GitEffectDispositionV1::Deny,
                vec![GitEffectReasonV1::CapabilityNotGranted],
            );
        }
        if !input.authorization.owner_scope_matches {
            return self.decision(
                input,
                GitEffectDispositionV1::Deny,
                vec![GitEffectReasonV1::OwnerScopeMismatch],
            );
        }
        if input.effect.requires_preview() && !input.repository_state.mutation_eligible {
            return self.decision(
                input,
                GitEffectDispositionV1::Deny,
                vec![GitEffectReasonV1::RepositoryStateIneligible],
            );
        }
        if input.effect.requires_preview() {
            let Some(expected_preview_digest) = &input.expected_preview_digest else {
                return self.decision(
                    input,
                    GitEffectDispositionV1::Deny,
                    vec![GitEffectReasonV1::PreviewRequired],
                );
            };
            let Some(preview) = &input.preview else {
                return self.decision(
                    input,
                    GitEffectDispositionV1::Deny,
                    vec![GitEffectReasonV1::PreviewRequired],
                );
            };
            if &preview.preview_digest != expected_preview_digest {
                return self.decision(
                    input,
                    GitEffectDispositionV1::Deny,
                    vec![GitEffectReasonV1::PreviewDigestMismatch],
                );
            }
            if preview.repository_state_id != input.repository_state.snapshot_id {
                return self.decision(
                    input,
                    GitEffectDispositionV1::Deny,
                    vec![GitEffectReasonV1::PreviewSnapshotMismatch],
                );
            }
        }
        match input.conflict_risk {
            GitConflictRiskV1::NoneKnown => self.decision(
                input,
                GitEffectDispositionV1::Allow,
                vec![GitEffectReasonV1::Classified],
            ),
            GitConflictRiskV1::Possible => self.decision(
                input,
                GitEffectDispositionV1::Indeterminate,
                vec![GitEffectReasonV1::PossibleConflict],
            ),
            GitConflictRiskV1::Confirmed => self.decision(
                input,
                GitEffectDispositionV1::Deny,
                vec![GitEffectReasonV1::ConfirmedConflict],
            ),
        }
    }
}
