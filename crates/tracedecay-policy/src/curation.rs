//! Pure admission for automatic curation effects.
//!
//! Callers provide the exact validation output, evidence, and configuration.
//! This evaluator never validates, mutates, or writes an effect receipt.

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{ConfigurationRevisionId, UserProfileId};
use tracedecay_domain::{ActorId, DomainError, ManifestDigest, ProjectId, canonical_sha256};

pub const CURATION_APPLY_EVALUATOR_ID_V1: &str = "tracedecay.curation-apply.v1";
pub const CURATION_APPLY_EVALUATOR_REVISION_V1: u64 = 1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurationApplySubjectV1 {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurationValidationDispositionV1 {
    Accepted,
    NoCandidate,
}

/// Immutable admission facts for one exact curation output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurationApplyAuthorityV1 {
    pub actor_id: ActorId,
    pub project_id: Option<ProjectId>,
    pub profile_id: UserProfileId,
    pub configuration_revision_id: ConfigurationRevisionId,
}

/// Immutable admission facts for one exact curation output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurationApplyPolicyInputV1 {
    pub authority: CurationApplyAuthorityV1,
    pub subject: CurationApplySubjectV1,
    pub evidence_digest: Option<ManifestDigest>,
    pub output_digest: ManifestDigest,
    pub validation: CurationValidationDispositionV1,
    pub configuration_digest: ManifestDigest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurationApplyDispositionV1 {
    Allow,
    Deny,
    NotApplicable,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CurationApplyReasonCodeV1 {
    InvalidInput,
    UnauthorizedActor,
    NoCandidate,
    EvidenceUnavailable,
    Allowed,
}

/// Recorded, replayable decision for one exact curation output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurationApplyDecisionV1 {
    pub evaluator_id: String,
    pub evaluator_revision: u64,
    pub evaluator_digest: ManifestDigest,
    pub input_digest: ManifestDigest,
    pub authority: CurationApplyAuthorityV1,
    pub subject: CurationApplySubjectV1,
    pub evidence_digest: Option<ManifestDigest>,
    pub output_digest: ManifestDigest,
    pub validation: CurationValidationDispositionV1,
    pub configuration_digest: ManifestDigest,
    pub disposition: CurationApplyDispositionV1,
    pub ordered_reason_codes: Vec<CurationApplyReasonCodeV1>,
    pub decision_digest: ManifestDigest,
}

impl CurationApplyDecisionV1 {
    pub fn allows_apply(&self) -> bool {
        matches!(self.disposition, CurationApplyDispositionV1::Allow)
    }
}

fn curation_apply_evaluator_digest() -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        CURATION_APPLY_EVALUATOR_ID_V1,
        CURATION_APPLY_EVALUATOR_REVISION_V1,
    ))
}

/// Evaluates whether a sealed curation output may be automatically applied.
pub fn evaluate_curation_apply(
    input: &CurationApplyPolicyInputV1,
) -> Result<CurationApplyDecisionV1, DomainError> {
    let input_digest = canonical_sha256(input)?;
    let authority_is_invalid = input.authority.actor_id.validate().is_err()
        || input.authority.profile_id.validate().is_err()
        || input
            .authority
            .configuration_revision_id
            .validate()
            .is_err()
        || input
            .authority
            .project_id
            .as_ref()
            .is_some_and(|project_id| project_id.validate().is_err());
    let actor_matches_subject = input.authority.actor_id.as_str()
        == match input.subject {
            CurationApplySubjectV1::MemoryCurator => "automation:memory-curator",
            CurationApplySubjectV1::SessionReflector => "automation:session-reflector",
            CurationApplySubjectV1::SkillWriter => "automation:skill-writer",
        };
    let (disposition, ordered_reason_codes) = if authority_is_invalid
        || input.output_digest.validate().is_err()
        || input.configuration_digest.validate().is_err()
        || input
            .evidence_digest
            .as_ref()
            .is_some_and(|digest| digest.validate().is_err())
    {
        (
            CurationApplyDispositionV1::Deny,
            vec![CurationApplyReasonCodeV1::InvalidInput],
        )
    } else if !actor_matches_subject {
        (
            CurationApplyDispositionV1::Deny,
            vec![CurationApplyReasonCodeV1::UnauthorizedActor],
        )
    } else if input.validation == CurationValidationDispositionV1::NoCandidate {
        (
            CurationApplyDispositionV1::NotApplicable,
            vec![CurationApplyReasonCodeV1::NoCandidate],
        )
    } else if input.evidence_digest.is_none() {
        (
            CurationApplyDispositionV1::Indeterminate,
            vec![CurationApplyReasonCodeV1::EvidenceUnavailable],
        )
    } else {
        (
            CurationApplyDispositionV1::Allow,
            vec![CurationApplyReasonCodeV1::Allowed],
        )
    };
    let evaluator_id = CURATION_APPLY_EVALUATOR_ID_V1.to_owned();
    let evaluator_revision = CURATION_APPLY_EVALUATOR_REVISION_V1;
    let evaluator_digest = curation_apply_evaluator_digest()?;
    let decision_digest = canonical_sha256(&(
        &evaluator_id,
        evaluator_revision,
        &evaluator_digest,
        &input_digest,
        &input.authority,
        input.subject,
        &input.evidence_digest,
        &input.output_digest,
        input.validation,
        &input.configuration_digest,
        disposition,
        &ordered_reason_codes,
    ))?;
    Ok(CurationApplyDecisionV1 {
        evaluator_id,
        evaluator_revision,
        evaluator_digest,
        input_digest,
        authority: input.authority.clone(),
        subject: input.subject,
        evidence_digest: input.evidence_digest.clone(),
        output_digest: input.output_digest.clone(),
        validation: input.validation,
        configuration_digest: input.configuration_digest.clone(),
        disposition,
        ordered_reason_codes,
        decision_digest,
    })
}
