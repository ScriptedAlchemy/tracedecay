//! Pure analyzer-admission evaluator.
//!
//! This module chooses only from explicit configured/cataloged candidates. It
//! never probes an executable, starts a process, reads host state, or invents
//! a fallback analyzer.

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    AnalyzerExecutableId, AnalyzerLanguageId, AnalyzerPrivacyClassV1, AnalyzerSettingsV1,
};
use tracedecay_domain::{CapabilityId, ManifestDigest, UtcMicros};

use crate::authorization::{
    PolicyIdentifierV1, PrivacyConstraintSetV1, PrivacyConstraintV1, policy_digest,
};

const ANALYZER_ADMISSION_SNAPSHOT_DOMAIN: &str = "tracedecay.policy.analyzer-admission-snapshot.v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerAvailabilityV1 {
    Available,
    Unavailable,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerExecutionLocationV1 {
    Local,
    External,
}

/// Immutable catalog/runtime observation supplied by the caller. Availability
/// is evidence, not a command to start, stop, or probe an analyzer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerCandidateV1 {
    pub executable_id: AnalyzerExecutableId,
    /// Present only when this catalog candidate represents the exact
    /// `ApprovedExternal` executable digest selected by configuration.
    pub approved_external_digest: Option<ManifestDigest>,
    pub language_id: AnalyzerLanguageId,
    pub capability_id: CapabilityId,
    pub availability: AnalyzerAvailabilityV1,
    pub execution_location: AnalyzerExecutionLocationV1,
    pub scope_authorized: bool,
    pub available_memory_mib: u32,
    pub catalog_digest: ManifestDigest,
}

impl AnalyzerCandidateV1 {
    fn is_valid(&self) -> bool {
        self.executable_id.validate().is_ok()
            && self
                .approved_external_digest
                .as_ref()
                .is_none_or(|digest| digest.validate().is_ok())
            && self.language_id.validate().is_ok()
            && self.capability_id.validate().is_ok()
            && self.catalog_digest.validate().is_ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerAdmissionInputV1 {
    pub settings: AnalyzerSettingsV1,
    pub language_id: AnalyzerLanguageId,
    pub requested_capability: CapabilityId,
    pub candidates: Vec<AnalyzerCandidateV1>,
    pub privacy_constraints: PrivacyConstraintSetV1,
    pub configuration_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub evaluated_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerAdmissionDispositionV1 {
    Allow,
    Deny,
    NotApplicable,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerAdmissionReasonV1 {
    InvalidSettings,
    NoEnabledLanguageSelection,
    NoConfiguredCandidate,
    CandidateUnavailable,
    CandidateStale,
    CandidateUnknown,
    CandidateAmbiguous,
    ScopeUnauthorized,
    LocalOnlyPrivacy,
    RestrictedPrivacy,
    InsufficientMemory,
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerAdmissionDecisionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub disposition: AnalyzerAdmissionDispositionV1,
    pub selected_executable_id: Option<AnalyzerExecutableId>,
    pub ordered_reason_codes: Vec<AnalyzerAdmissionReasonV1>,
}

impl AnalyzerAdmissionDecisionV1 {
    /// Verifies that a decision is bound to the exact immutable policy and
    /// configuration input that produced it. This is deliberately a binding
    /// check, not a request to re-run or supervise an analyzer.
    pub fn is_bound_to(&self, input: &AnalyzerAdmissionInputV1) -> bool {
        let selected_is_consistent = match self.disposition {
            AnalyzerAdmissionDispositionV1::Allow => self.selected_executable_id.is_some(),
            AnalyzerAdmissionDispositionV1::Deny
            | AnalyzerAdmissionDispositionV1::NotApplicable
            | AnalyzerAdmissionDispositionV1::Indeterminate => {
                self.selected_executable_id.is_none()
            }
        };
        self.evaluator_id.is_valid()
            && self.evaluator_revision > 0
            && self.input_digest
                == policy_digest("tracedecay.policy.analyzer-admission-input.v1", input)
            && self.policy_revision == input.policy_revision
            && self.policy_digest == input.policy_digest
            && self.configuration_digest == input.configuration_digest
            && self.policy_digest.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
            && !self.ordered_reason_codes.is_empty()
            && selected_is_consistent
    }
}

/// Immutable policy portion of a runtime analyzer snapshot. Plan 35 can
/// compose this value with independently-owned provider/runtime observations
/// without copying policy/configuration digest semantics or creating an
/// analyzer lifecycle authority in policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerAdmissionSnapshotV1 {
    pub decision: AnalyzerAdmissionDecisionV1,
    pub snapshot_digest: ManifestDigest,
}

impl AnalyzerAdmissionSnapshotV1 {
    pub fn compose<E>(evaluator: &E, input: &AnalyzerAdmissionInputV1) -> Self
    where
        E: AnalyzerAdmissionEvaluator,
    {
        let decision = evaluator.evaluate(input);
        let snapshot_digest = policy_digest(ANALYZER_ADMISSION_SNAPSHOT_DOMAIN, &decision);
        Self {
            decision,
            snapshot_digest,
        }
    }

    pub fn is_bound_to(&self, input: &AnalyzerAdmissionInputV1) -> bool {
        self.snapshot_digest == policy_digest(ANALYZER_ADMISSION_SNAPSHOT_DOMAIN, &self.decision)
            && self.snapshot_digest.validate().is_ok()
            && self.decision.is_bound_to(input)
    }
}

pub trait AnalyzerAdmissionEvaluator {
    fn evaluate(&self, input: &AnalyzerAdmissionInputV1) -> AnalyzerAdmissionDecisionV1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyzerAdmissionEvaluatorV1 {
    evaluator_id: PolicyIdentifierV1,
}

impl Default for AnalyzerAdmissionEvaluatorV1 {
    fn default() -> Self {
        Self {
            evaluator_id: PolicyIdentifierV1::new("analyzer_admission.v1")
                .expect("static evaluator identifier is valid"),
        }
    }
}

impl AnalyzerAdmissionEvaluatorV1 {
    /// Revision of this reviewed implementation, recorded with every decision
    /// so replay can refuse a substituted evaluator. It is a property of the
    /// code, not of an instance.
    const EVALUATOR_REVISION: u64 = 1;

    pub fn snapshot(&self, input: &AnalyzerAdmissionInputV1) -> AnalyzerAdmissionSnapshotV1 {
        AnalyzerAdmissionSnapshotV1::compose(self, input)
    }

    fn decision(
        &self,
        input: &AnalyzerAdmissionInputV1,
        disposition: AnalyzerAdmissionDispositionV1,
        selected_executable_id: Option<AnalyzerExecutableId>,
        ordered_reason_codes: Vec<AnalyzerAdmissionReasonV1>,
    ) -> AnalyzerAdmissionDecisionV1 {
        AnalyzerAdmissionDecisionV1 {
            evaluator_id: self.evaluator_id.clone(),
            evaluator_revision: Self::EVALUATOR_REVISION,
            input_digest: policy_digest("tracedecay.policy.analyzer-admission-input.v1", input),
            policy_revision: input.policy_revision,
            policy_digest: input.policy_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            disposition,
            selected_executable_id,
            ordered_reason_codes,
        }
    }
}

impl AnalyzerAdmissionEvaluator for AnalyzerAdmissionEvaluatorV1 {
    fn evaluate(&self, input: &AnalyzerAdmissionInputV1) -> AnalyzerAdmissionDecisionV1 {
        if input.settings.validate().is_err()
            || input.language_id.validate().is_err()
            || input.requested_capability.validate().is_err()
            || input.configuration_digest.validate().is_err()
            || input.policy_digest.validate().is_err()
            || input.policy_revision == 0
            || input
                .candidates
                .iter()
                .any(|candidate| !candidate.is_valid())
        {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::InvalidSettings],
            );
        }

        let Some(selection) = input
            .settings
            .selections
            .iter()
            .find(|selection| selection.language_id == input.language_id && selection.enabled)
        else {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::NotApplicable,
                None,
                vec![AnalyzerAdmissionReasonV1::NoEnabledLanguageSelection],
            );
        };

        let candidates = input
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.language_id == input.language_id
                    && candidate.capability_id == input.requested_capability
                    && match &selection.executable {
                        tracedecay_domain::configuration::AnalyzerExecutableReferenceV1::BuiltIn {
                            executable_id,
                        } => &candidate.executable_id == executable_id,
                        tracedecay_domain::configuration::AnalyzerExecutableReferenceV1::ApprovedExternal {
                            executable_digest,
                        } => {
                            candidate.approved_external_digest.as_ref() == Some(executable_digest)
                        }
                    }
            })
            .collect::<Vec<_>>();
        let Some(candidate) = candidates.first().copied() else {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::NotApplicable,
                None,
                vec![AnalyzerAdmissionReasonV1::NoConfiguredCandidate],
            );
        };
        if candidates.len() != 1 {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::CandidateAmbiguous],
            );
        }

        if !candidate.scope_authorized {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Deny,
                None,
                vec![AnalyzerAdmissionReasonV1::ScopeUnauthorized],
            );
        }
        if input
            .privacy_constraints
            .contains(&PrivacyConstraintV1::LocalOnly)
            && candidate.execution_location == AnalyzerExecutionLocationV1::External
        {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Deny,
                None,
                vec![AnalyzerAdmissionReasonV1::LocalOnlyPrivacy],
            );
        }
        if selection.privacy_class == AnalyzerPrivacyClassV1::Restricted
            && candidate.execution_location == AnalyzerExecutionLocationV1::External
        {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Deny,
                None,
                vec![AnalyzerAdmissionReasonV1::RestrictedPrivacy],
            );
        }
        if candidate.available_memory_mib < selection.resource_limits.maximum_memory_mib {
            return self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::InsufficientMemory],
            );
        }
        match candidate.availability {
            AnalyzerAvailabilityV1::Available => self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Allow,
                Some(candidate.executable_id.clone()),
                vec![AnalyzerAdmissionReasonV1::Selected],
            ),
            AnalyzerAvailabilityV1::Unavailable => self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::CandidateUnavailable],
            ),
            AnalyzerAvailabilityV1::Stale => self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::CandidateStale],
            ),
            AnalyzerAvailabilityV1::Unknown => self.decision(
                input,
                AnalyzerAdmissionDispositionV1::Indeterminate,
                None,
                vec![AnalyzerAdmissionReasonV1::CandidateUnknown],
            ),
        }
    }
}
