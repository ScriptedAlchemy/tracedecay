//! Pure capability routing over explicit catalog and grant facts.
//!
//! A route is selected only from caller-declared capability order. Missing or
//! unavailable capabilities never cause an inferred fallback.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CapabilityId, ManifestDigest, UtcMicros};

use crate::authorization::{PolicyIdentifierV1, policy_digest};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailabilityV1 {
    Available,
    Unavailable,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatchV1 {
    Match,
    Mismatch,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffectClassV1 {
    Read,
    Preview,
    Advisory,
    GitIndexStage,
    GitIndexUnstage,
    GitIndexCommit,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TruthSourceStateV1 {
    Fresh,
    Partial,
    Stale,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TruthFreshnessRequirementV1 {
    Fresh,
    FreshOrPartial,
}

impl TruthFreshnessRequirementV1 {
    fn accepts(self, state: TruthSourceStateV1) -> bool {
        matches!(
            (self, state),
            (Self::Fresh, TruthSourceStateV1::Fresh)
                | (
                    Self::FreshOrPartial,
                    TruthSourceStateV1::Fresh | TruthSourceStateV1::Partial
                )
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRoutingGrantStateV1 {
    Active,
    Revoked,
    Stale,
    Ambiguous,
}

/// One immutable, current grant snapshot supplied by the application
/// authority. Policy can narrow or reject it, but cannot issue or refresh it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRoutingGrantV1 {
    pub grant_id: PolicyIdentifierV1,
    pub revision: u64,
    pub digest: ManifestDigest,
    pub allowed_capabilities: BTreeSet<CapabilityId>,
    pub allowed_use_cases: BTreeSet<PolicyIdentifierV1>,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub state: CapabilityRoutingGrantStateV1,
}

impl CapabilityRoutingGrantV1 {
    fn is_valid(&self) -> bool {
        self.grant_id.is_valid()
            && self.revision > 0
            && self.digest.validate().is_ok()
            && !self.allowed_capabilities.is_empty()
            && self
                .allowed_capabilities
                .iter()
                .all(|capability| capability.validate().is_ok())
            && !self.allowed_use_cases.is_empty()
            && self
                .allowed_use_cases
                .iter()
                .all(PolicyIdentifierV1::is_valid)
            && self.issued_at < self.expires_at
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityRoutingCancellationV1 {
    Active,
    Cancelled { requested_at: UtcMicros },
}

/// A catalog-projected candidate. The catalog/runtime owner provides every
/// availability and truth-source fact; this crate does not inspect one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRouteCandidateV1 {
    pub capability_id: CapabilityId,
    pub use_case_id: PolicyIdentifierV1,
    pub availability: CapabilityAvailabilityV1,
    pub scope_match: ScopeMatchV1,
    pub effect_class: CapabilityEffectClassV1,
    pub truth_source_state: TruthSourceStateV1,
    pub catalog_revision: u64,
    pub catalog_digest: ManifestDigest,
    pub capability_digest: ManifestDigest,
}

impl CapabilityRouteCandidateV1 {
    fn is_valid(&self) -> bool {
        self.capability_id.validate().is_ok()
            && self.use_case_id.is_valid()
            && self.catalog_revision > 0
            && self.catalog_digest.validate().is_ok()
            && self.capability_digest.validate().is_ok()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRoutingRequestV1 {
    pub requested_use_case_id: PolicyIdentifierV1,
    /// Ordered by explicit caller/catalog declaration. This is the only
    /// fallback relation policy may consider.
    pub declared_capability_order: Vec<CapabilityId>,
    pub candidates: Vec<CapabilityRouteCandidateV1>,
    pub grant: CapabilityRoutingGrantV1,
    pub required_effect_class: CapabilityEffectClassV1,
    pub required_freshness: TruthFreshnessRequirementV1,
    pub catalog_revision: u64,
    pub catalog_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub deadline: UtcMicros,
    pub cancellation: CapabilityRoutingCancellationV1,
    pub evaluated_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRoutingDispositionV1 {
    Allow,
    Deny,
    NotApplicable,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityRoutingReasonV1 {
    InvalidRequest,
    RequestCancelled,
    DeadlineExceeded,
    GrantRevoked,
    GrantStale,
    GrantAmbiguous,
    GrantNotYetIssued,
    GrantExpired,
    UseCaseNotAuthorized,
    CapabilityNotAuthorized,
    CatalogSnapshotMismatch,
    CandidateUseCaseMismatch,
    CapabilityUnavailable,
    CapabilityStale,
    CapabilityUnknown,
    ScopeMismatch,
    EffectMismatch,
    TruthNotFresh,
    DeclaredCandidateMissing,
    CandidateAmbiguous,
    Selected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRoutingDecisionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub requested_use_case_id: PolicyIdentifierV1,
    pub grant_id: PolicyIdentifierV1,
    pub grant_revision: u64,
    pub grant_digest: ManifestDigest,
    pub catalog_revision: u64,
    pub catalog_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub disposition: CapabilityRoutingDispositionV1,
    pub selected_capability_id: Option<CapabilityId>,
    pub ordered_reason_codes: Vec<CapabilityRoutingReasonV1>,
}

pub trait CapabilityRoutingEvaluator {
    fn evaluate(&self, request: &CapabilityRoutingRequestV1) -> CapabilityRoutingDecisionV1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityRoutingEvaluatorV1 {
    evaluator_id: PolicyIdentifierV1,
}

impl Default for CapabilityRoutingEvaluatorV1 {
    fn default() -> Self {
        Self {
            evaluator_id: PolicyIdentifierV1::new("capability_routing.v1")
                .expect("static evaluator identifier is valid"),
        }
    }
}

impl CapabilityRoutingEvaluatorV1 {
    /// Revision of this reviewed implementation, recorded with every decision
    /// so replay can refuse a substituted evaluator. It is a property of the
    /// code, not of an instance.
    const EVALUATOR_REVISION: u64 = 1;

    fn decision(
        &self,
        request: &CapabilityRoutingRequestV1,
        disposition: CapabilityRoutingDispositionV1,
        selected_capability_id: Option<CapabilityId>,
        ordered_reason_codes: Vec<CapabilityRoutingReasonV1>,
    ) -> CapabilityRoutingDecisionV1 {
        CapabilityRoutingDecisionV1 {
            evaluator_id: self.evaluator_id.clone(),
            evaluator_revision: Self::EVALUATOR_REVISION,
            input_digest: policy_digest("tracedecay.policy.capability-routing-input.v1", request),
            requested_use_case_id: request.requested_use_case_id.clone(),
            grant_id: request.grant.grant_id.clone(),
            grant_revision: request.grant.revision,
            grant_digest: request.grant.digest.clone(),
            catalog_revision: request.catalog_revision,
            catalog_digest: request.catalog_digest.clone(),
            policy_revision: request.policy_revision,
            policy_digest: request.policy_digest.clone(),
            configuration_digest: request.configuration_digest.clone(),
            disposition,
            selected_capability_id,
            ordered_reason_codes,
        }
    }
}

impl CapabilityRoutingEvaluator for CapabilityRoutingEvaluatorV1 {
    fn evaluate(&self, request: &CapabilityRoutingRequestV1) -> CapabilityRoutingDecisionV1 {
        let mut declared = BTreeSet::new();
        if !request.requested_use_case_id.is_valid()
            || request.declared_capability_order.is_empty()
            || !request.grant.is_valid()
            || request.catalog_revision == 0
            || request.catalog_digest.validate().is_err()
            || request.policy_revision == 0
            || request.policy_digest.validate().is_err()
            || request.configuration_digest.validate().is_err()
            || request
                .candidates
                .iter()
                .any(|candidate| !candidate.is_valid())
            || request
                .declared_capability_order
                .iter()
                .any(|capability| capability.validate().is_err() || !declared.insert(capability))
        {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Indeterminate,
                None,
                vec![CapabilityRoutingReasonV1::InvalidRequest],
            );
        }
        if matches!(
            request.cancellation,
            CapabilityRoutingCancellationV1::Cancelled { .. }
        ) {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Indeterminate,
                None,
                vec![CapabilityRoutingReasonV1::RequestCancelled],
            );
        }
        if request.evaluated_at >= request.deadline {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Indeterminate,
                None,
                vec![CapabilityRoutingReasonV1::DeadlineExceeded],
            );
        }
        match request.grant.state {
            CapabilityRoutingGrantStateV1::Revoked => {
                return self.decision(
                    request,
                    CapabilityRoutingDispositionV1::Deny,
                    None,
                    vec![CapabilityRoutingReasonV1::GrantRevoked],
                );
            }
            CapabilityRoutingGrantStateV1::Stale => {
                return self.decision(
                    request,
                    CapabilityRoutingDispositionV1::Indeterminate,
                    None,
                    vec![CapabilityRoutingReasonV1::GrantStale],
                );
            }
            CapabilityRoutingGrantStateV1::Ambiguous => {
                return self.decision(
                    request,
                    CapabilityRoutingDispositionV1::Indeterminate,
                    None,
                    vec![CapabilityRoutingReasonV1::GrantAmbiguous],
                );
            }
            CapabilityRoutingGrantStateV1::Active => {}
        }
        if request.evaluated_at < request.grant.issued_at {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Deny,
                None,
                vec![CapabilityRoutingReasonV1::GrantNotYetIssued],
            );
        }
        if request.evaluated_at >= request.grant.expires_at {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Deny,
                None,
                vec![CapabilityRoutingReasonV1::GrantExpired],
            );
        }
        if !request
            .grant
            .allowed_use_cases
            .contains(&request.requested_use_case_id)
        {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Deny,
                None,
                vec![CapabilityRoutingReasonV1::UseCaseNotAuthorized],
            );
        }
        if request.candidates.iter().any(|candidate| {
            candidate.catalog_revision != request.catalog_revision
                || candidate.catalog_digest != request.catalog_digest
        }) {
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Indeterminate,
                None,
                vec![CapabilityRoutingReasonV1::CatalogSnapshotMismatch],
            );
        }

        let mut saw_unavailable = false;
        let mut saw_denied = false;
        let mut reasons = Vec::new();
        for capability_id in &request.declared_capability_order {
            let candidates = request
                .candidates
                .iter()
                .filter(|candidate| &candidate.capability_id == capability_id)
                .collect::<Vec<_>>();
            let Some(candidate) = candidates.first().copied() else {
                reasons.push(CapabilityRoutingReasonV1::DeclaredCandidateMissing);
                saw_unavailable = true;
                continue;
            };
            if candidates.len() != 1 {
                reasons.push(CapabilityRoutingReasonV1::CandidateAmbiguous);
                saw_unavailable = true;
                continue;
            }
            if candidate.use_case_id != request.requested_use_case_id {
                reasons.push(CapabilityRoutingReasonV1::CandidateUseCaseMismatch);
                saw_denied = true;
                continue;
            }
            if !request.grant.allowed_capabilities.contains(capability_id) {
                reasons.push(CapabilityRoutingReasonV1::CapabilityNotAuthorized);
                saw_denied = true;
                continue;
            }
            match candidate.availability {
                CapabilityAvailabilityV1::Available => {}
                CapabilityAvailabilityV1::Unavailable => {
                    reasons.push(CapabilityRoutingReasonV1::CapabilityUnavailable);
                    saw_unavailable = true;
                    continue;
                }
                CapabilityAvailabilityV1::Stale => {
                    reasons.push(CapabilityRoutingReasonV1::CapabilityStale);
                    saw_unavailable = true;
                    continue;
                }
                CapabilityAvailabilityV1::Unknown => {
                    reasons.push(CapabilityRoutingReasonV1::CapabilityUnknown);
                    saw_unavailable = true;
                    continue;
                }
            }
            if candidate.scope_match != ScopeMatchV1::Match {
                reasons.push(CapabilityRoutingReasonV1::ScopeMismatch);
                saw_denied = true;
                continue;
            }
            if candidate.effect_class != request.required_effect_class {
                reasons.push(CapabilityRoutingReasonV1::EffectMismatch);
                saw_denied = true;
                continue;
            }
            if !request
                .required_freshness
                .accepts(candidate.truth_source_state)
            {
                reasons.push(CapabilityRoutingReasonV1::TruthNotFresh);
                saw_unavailable = true;
                continue;
            }
            reasons.push(CapabilityRoutingReasonV1::Selected);
            return self.decision(
                request,
                CapabilityRoutingDispositionV1::Allow,
                Some(capability_id.clone()),
                reasons,
            );
        }

        let disposition = if saw_unavailable {
            CapabilityRoutingDispositionV1::Indeterminate
        } else if saw_denied {
            CapabilityRoutingDispositionV1::Deny
        } else {
            CapabilityRoutingDispositionV1::NotApplicable
        };
        self.decision(request, disposition, None, reasons)
    }
}
