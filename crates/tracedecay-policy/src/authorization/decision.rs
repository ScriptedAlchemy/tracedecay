use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;

use super::grant::GrantStateAtV1;
use super::input::{
    AuthorizationCoverageV1, AuthorizationSnapshotStateV1, ExternalContentStatusV1,
    PolicyIdentifierV1, SourceAuthorizationInputV1, policy_digest,
};
use super::intersection::{
    EffectiveSourceGrantV1, IntersectionFailureV1, intersect_source_authority,
};
use super::state::{
    PublicSourceResultShapeV1, SourceAccessDecisionV1, SourceAuthorizationDispositionV1,
};

/// Stable evaluator implementation identity. It is recorded with every
/// decision so exact replay can refuse a substituted evaluator revision.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvaluatorVersionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
}

impl PolicyEvaluatorVersionV1 {
    pub fn is_valid(&self) -> bool {
        self.evaluator_id.is_valid() && self.evaluator_revision > 0
    }
}

/// Stable machine-readable decision trace entries. Renderers may turn these
/// into text, but text never changes the authority represented by a decision.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReasonCodeV1 {
    InputInvalid,
    InputComplete,
    InputPartial,
    InputMissing,
    InputStale,
    InputAmbiguous,
    SourceDefinitionBindingMismatch,
    SourcePolicySourceMismatch,
    SinkPolicySinkMismatch,
    OwnerScopeMismatch,
    RequesterSubjectMismatch,
    OperationPolicyExcluded,
    SinkPolicyExcluded,
    SourceGrantActive,
    SourceGrantRevoked,
    SourceGrantStale,
    SourceGrantAmbiguous,
    SourceGrantNotYetIssued,
    SourceGrantExpired,
    RequesterGrantActive,
    RequesterGrantRevoked,
    RequesterGrantStale,
    RequesterGrantAmbiguous,
    RequesterGrantNotYetIssued,
    RequesterGrantExpired,
    GrantIntersectionNonExpanding,
    ResourceNotGranted,
    OperationNotGranted,
    SinkNotGranted,
    DisclosureTooBroad,
    BudgetExceeded,
    MandatoryLocalPrivacyBlocksEgress,
    SanitizedOnlyBlocksDisclosure,
    NoModelContext,
    NoRetention,
    NoTelemetry,
    NoExport,
    SinkUnavailable,
    AccessAllowed,
    AuthorizationCoveragePartial,
    ContentLive,
    ContentPartial,
    ContentTemporarilyUnavailable,
    ContentAuthoritativeDeleted,
    SinkPolicyDrift,
    AuthorizationInputDrift,
}

/// Decision trace over immutable source authorization facts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationDecisionV1 {
    pub evaluator_version: PolicyEvaluatorVersionV1,
    pub input_digest: ManifestDigest,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub content_status: ExternalContentStatusV1,
    pub access: SourceAccessDecisionV1,
    pub authorization_coverage: AuthorizationCoverageV1,
    pub disposition: SourceAuthorizationDispositionV1,
    pub effective_grant: Option<EffectiveSourceGrantV1>,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
    pub evidence_references: Vec<PolicyIdentifierV1>,
    pub decision_digest: ManifestDigest,
}

impl SourceAuthorizationDecisionV1 {
    pub fn is_authorized(&self) -> bool {
        self.access == SourceAccessDecisionV1::Authorized
    }

    fn compute_decision_digest(&self) -> ManifestDigest {
        #[derive(Serialize)]
        struct DecisionMaterial<'a> {
            evaluator_version: &'a PolicyEvaluatorVersionV1,
            input_digest: &'a ManifestDigest,
            policy_revision: u64,
            policy_digest: &'a ManifestDigest,
            configuration_digest: &'a ManifestDigest,
            content_status: ExternalContentStatusV1,
            access: SourceAccessDecisionV1,
            authorization_coverage: AuthorizationCoverageV1,
            disposition: SourceAuthorizationDispositionV1,
            effective_grant: &'a Option<EffectiveSourceGrantV1>,
            ordered_reason_codes: &'a [PolicyReasonCodeV1],
            evidence_references: &'a [PolicyIdentifierV1],
        }

        policy_digest(
            "tracedecay.policy.source-authorization-decision.v1",
            &DecisionMaterial {
                evaluator_version: &self.evaluator_version,
                input_digest: &self.input_digest,
                policy_revision: self.policy_revision,
                policy_digest: &self.policy_digest,
                configuration_digest: &self.configuration_digest,
                content_status: self.content_status,
                access: self.access,
                authorization_coverage: self.authorization_coverage,
                disposition: self.disposition,
                effective_grant: &self.effective_grant,
                ordered_reason_codes: &self.ordered_reason_codes,
                evidence_references: &self.evidence_references,
            },
        )
    }
}

/// Expected JSON truth-table projection. It intentionally asserts only public
/// stable semantics, not opaque digest bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationExpectedDecisionV1 {
    pub access: SourceAccessDecisionV1,
    pub authorization_coverage: AuthorizationCoverageV1,
    pub disposition: SourceAuthorizationDispositionV1,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
    pub has_effective_grant: bool,
    pub public_shape: PublicSourceResultShapeV1,
}

/// Checked-in JSON truth-table row for the deterministic source evaluator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationTruthTableV1 {
    pub name: String,
    pub source_visible: bool,
    pub input: SourceAuthorizationInputV1,
    pub expected: SourceAuthorizationExpectedDecisionV1,
}

/// Pure source authorization contract.
pub trait SourceAuthorizationEvaluator {
    fn evaluator_version(&self) -> &PolicyEvaluatorVersionV1;

    fn evaluate(&self, input: &SourceAuthorizationInputV1) -> SourceAuthorizationDecisionV1;
}

/// Reviewed Rust implementation of source authorization. It has no mutable
/// state and therefore evaluates identical input bytes identically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceAuthorizationEvaluatorV1 {
    version: PolicyEvaluatorVersionV1,
}

impl Default for SourceAuthorizationEvaluatorV1 {
    fn default() -> Self {
        Self {
            version: PolicyEvaluatorVersionV1 {
                evaluator_id: PolicyIdentifierV1::new("source_authorization.v1")
                    .expect("static evaluator identifier is valid"),
                evaluator_revision: 1,
            },
        }
    }
}

impl SourceAuthorizationEvaluatorV1 {
    pub fn version(&self) -> &PolicyEvaluatorVersionV1 {
        &self.version
    }

    fn decision(
        &self,
        input: &SourceAuthorizationInputV1,
        access: SourceAccessDecisionV1,
        coverage: AuthorizationCoverageV1,
        disposition: SourceAuthorizationDispositionV1,
        effective_grant: Option<EffectiveSourceGrantV1>,
        ordered_reason_codes: Vec<PolicyReasonCodeV1>,
    ) -> SourceAuthorizationDecisionV1 {
        let mut decision = SourceAuthorizationDecisionV1 {
            evaluator_version: self.version.clone(),
            input_digest: input.input_digest(),
            policy_revision: input.policy_revision,
            policy_digest: input.policy_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            content_status: input.content_status,
            access,
            authorization_coverage: coverage,
            disposition,
            effective_grant,
            ordered_reason_codes,
            evidence_references: input.evidence_references.iter().cloned().collect(),
            decision_digest: policy_digest(
                "tracedecay.policy.source-authorization-decision.pending.v1",
                &input.input_digest(),
            ),
        };
        decision.decision_digest = decision.compute_decision_digest();
        decision
    }

    fn non_authorizing(
        &self,
        input: &SourceAuthorizationInputV1,
        access: SourceAccessDecisionV1,
        disposition: SourceAuthorizationDispositionV1,
        reasons: Vec<PolicyReasonCodeV1>,
    ) -> SourceAuthorizationDecisionV1 {
        self.decision(
            input,
            access,
            input.requested_coverage,
            disposition,
            None,
            reasons,
        )
    }

    fn grant_reason(
        source_grant: bool,
        state: GrantStateAtV1,
    ) -> (PolicyReasonCodeV1, SourceAuthorizationDispositionV1) {
        match (source_grant, state) {
            (true, GrantStateAtV1::Active) => (
                PolicyReasonCodeV1::SourceGrantActive,
                SourceAuthorizationDispositionV1::Allow,
            ),
            (false, GrantStateAtV1::Active) => (
                PolicyReasonCodeV1::RequesterGrantActive,
                SourceAuthorizationDispositionV1::Allow,
            ),
            (true, GrantStateAtV1::Revoked) => (
                PolicyReasonCodeV1::SourceGrantRevoked,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (false, GrantStateAtV1::Revoked) => (
                PolicyReasonCodeV1::RequesterGrantRevoked,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (true, GrantStateAtV1::Expired) => (
                PolicyReasonCodeV1::SourceGrantExpired,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (false, GrantStateAtV1::Expired) => (
                PolicyReasonCodeV1::RequesterGrantExpired,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (true, GrantStateAtV1::NotYetIssued) => (
                PolicyReasonCodeV1::SourceGrantNotYetIssued,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (false, GrantStateAtV1::NotYetIssued) => (
                PolicyReasonCodeV1::RequesterGrantNotYetIssued,
                SourceAuthorizationDispositionV1::Deny,
            ),
            (true, GrantStateAtV1::Stale) => (
                PolicyReasonCodeV1::SourceGrantStale,
                SourceAuthorizationDispositionV1::Indeterminate,
            ),
            (false, GrantStateAtV1::Stale) => (
                PolicyReasonCodeV1::RequesterGrantStale,
                SourceAuthorizationDispositionV1::Indeterminate,
            ),
            (true, GrantStateAtV1::Ambiguous) => (
                PolicyReasonCodeV1::SourceGrantAmbiguous,
                SourceAuthorizationDispositionV1::Indeterminate,
            ),
            (false, GrantStateAtV1::Ambiguous) => (
                PolicyReasonCodeV1::RequesterGrantAmbiguous,
                SourceAuthorizationDispositionV1::Indeterminate,
            ),
        }
    }

    fn intersection_reason(failure: IntersectionFailureV1) -> PolicyReasonCodeV1 {
        match failure {
            IntersectionFailureV1::OwnerMismatch => PolicyReasonCodeV1::OwnerScopeMismatch,
            IntersectionFailureV1::RequesterSubjectMismatch => {
                PolicyReasonCodeV1::RequesterSubjectMismatch
            }
            IntersectionFailureV1::ResourceNotGranted => PolicyReasonCodeV1::ResourceNotGranted,
            IntersectionFailureV1::OperationNotGranted => PolicyReasonCodeV1::OperationNotGranted,
            IntersectionFailureV1::SinkNotGranted => PolicyReasonCodeV1::SinkNotGranted,
            IntersectionFailureV1::DisclosureTooBroad => PolicyReasonCodeV1::DisclosureTooBroad,
            IntersectionFailureV1::BudgetExceeded => PolicyReasonCodeV1::BudgetExceeded,
            IntersectionFailureV1::MandatoryLocalPrivacyBlocksEgress => {
                PolicyReasonCodeV1::MandatoryLocalPrivacyBlocksEgress
            }
            IntersectionFailureV1::SanitizedOnlyBlocksDisclosure => {
                PolicyReasonCodeV1::SanitizedOnlyBlocksDisclosure
            }
            IntersectionFailureV1::NoModelContext => PolicyReasonCodeV1::NoModelContext,
            IntersectionFailureV1::NoRetention => PolicyReasonCodeV1::NoRetention,
            IntersectionFailureV1::NoTelemetry => PolicyReasonCodeV1::NoTelemetry,
            IntersectionFailureV1::NoExport => PolicyReasonCodeV1::NoExport,
            IntersectionFailureV1::SinkUnavailable => PolicyReasonCodeV1::SinkUnavailable,
        }
    }
}

impl SourceAuthorizationEvaluator for SourceAuthorizationEvaluatorV1 {
    fn evaluator_version(&self) -> &PolicyEvaluatorVersionV1 {
        &self.version
    }

    fn evaluate(&self, input: &SourceAuthorizationInputV1) -> SourceAuthorizationDecisionV1 {
        if !input.is_structurally_valid() || !self.version.is_valid() {
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                SourceAuthorizationDispositionV1::Indeterminate,
                vec![PolicyReasonCodeV1::InputInvalid],
            );
        }

        let mut reasons = Vec::new();
        match input.snapshot_state {
            AuthorizationSnapshotStateV1::Complete => {
                reasons.push(PolicyReasonCodeV1::InputComplete);
            }
            AuthorizationSnapshotStateV1::Partial => {
                return self.non_authorizing(
                    input,
                    SourceAccessDecisionV1::Unauthorized,
                    SourceAuthorizationDispositionV1::Indeterminate,
                    vec![PolicyReasonCodeV1::InputPartial],
                );
            }
            AuthorizationSnapshotStateV1::Missing => {
                return self.non_authorizing(
                    input,
                    SourceAccessDecisionV1::Unauthorized,
                    SourceAuthorizationDispositionV1::Indeterminate,
                    vec![PolicyReasonCodeV1::InputMissing],
                );
            }
            AuthorizationSnapshotStateV1::Stale => {
                return self.non_authorizing(
                    input,
                    SourceAccessDecisionV1::Unauthorized,
                    SourceAuthorizationDispositionV1::Indeterminate,
                    vec![PolicyReasonCodeV1::InputStale],
                );
            }
            AuthorizationSnapshotStateV1::Ambiguous => {
                return self.non_authorizing(
                    input,
                    SourceAccessDecisionV1::Unauthorized,
                    SourceAuthorizationDispositionV1::Indeterminate,
                    vec![PolicyReasonCodeV1::InputAmbiguous],
                );
            }
        }

        if &input.definition.definition.source_id != input.binding.binding.source_id() {
            reasons.push(PolicyReasonCodeV1::SourceDefinitionBindingMismatch);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                SourceAuthorizationDispositionV1::Deny,
                reasons,
            );
        }
        if input.definition.definition.source_id != input.source_policy.source_id {
            reasons.push(PolicyReasonCodeV1::SourcePolicySourceMismatch);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                SourceAuthorizationDispositionV1::Deny,
                reasons,
            );
        }
        if input.sink_policy.sink != input.requested_access.sink {
            reasons.push(PolicyReasonCodeV1::SinkPolicySinkMismatch);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                SourceAuthorizationDispositionV1::Deny,
                reasons,
            );
        }
        let binding_owner = input.binding.binding.owner();
        let resolved_owner = &input.resolved_owner_scope.owner;
        if &binding_owner != resolved_owner
            || &input.source_grant.owner != resolved_owner
            || &input.requester_grant.owner != resolved_owner
        {
            reasons.push(PolicyReasonCodeV1::OwnerScopeMismatch);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                SourceAuthorizationDispositionV1::Deny,
                reasons,
            );
        }
        if !input
            .source_policy
            .eligible_operations
            .contains(&input.requested_access.operation)
        {
            reasons.push(PolicyReasonCodeV1::OperationPolicyExcluded);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::PolicyExcluded,
                SourceAuthorizationDispositionV1::NotApplicable,
                reasons,
            );
        }
        if !input
            .source_policy
            .eligible_sinks
            .contains(&input.requested_access.sink)
        {
            reasons.push(PolicyReasonCodeV1::SinkPolicyExcluded);
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::PolicyExcluded,
                SourceAuthorizationDispositionV1::NotApplicable,
                reasons,
            );
        }

        let (source_reason, source_disposition) =
            Self::grant_reason(true, input.source_grant.state_at(input.evaluated_at));
        reasons.push(source_reason);
        if source_disposition != SourceAuthorizationDispositionV1::Allow {
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                source_disposition,
                reasons,
            );
        }
        let (requester_reason, requester_disposition) =
            Self::grant_reason(false, input.requester_grant.state_at(input.evaluated_at));
        reasons.push(requester_reason);
        if requester_disposition != SourceAuthorizationDispositionV1::Allow {
            return self.non_authorizing(
                input,
                SourceAccessDecisionV1::Unauthorized,
                requester_disposition,
                reasons,
            );
        }

        reasons.push(PolicyReasonCodeV1::GrantIntersectionNonExpanding);
        let effective_grant = match intersect_source_authority(input) {
            Ok(grant) => grant,
            Err(failure) => {
                let reason = Self::intersection_reason(failure);
                reasons.push(reason);
                let disposition = if failure == IntersectionFailureV1::SinkUnavailable {
                    SourceAuthorizationDispositionV1::Indeterminate
                } else {
                    SourceAuthorizationDispositionV1::Deny
                };
                return self.non_authorizing(
                    input,
                    SourceAccessDecisionV1::Unauthorized,
                    disposition,
                    reasons,
                );
            }
        };
        reasons.push(PolicyReasonCodeV1::AccessAllowed);

        let coverage = match (input.requested_coverage, input.content_status) {
            (AuthorizationCoverageV1::Partial, _) | (_, ExternalContentStatusV1::Partial) => {
                reasons.push(PolicyReasonCodeV1::AuthorizationCoveragePartial);
                AuthorizationCoverageV1::Partial
            }
            (AuthorizationCoverageV1::Complete, _) => AuthorizationCoverageV1::Complete,
        };
        let (disposition, content_reason) = match input.content_status {
            ExternalContentStatusV1::Live => (
                SourceAuthorizationDispositionV1::Allow,
                PolicyReasonCodeV1::ContentLive,
            ),
            ExternalContentStatusV1::Partial => (
                SourceAuthorizationDispositionV1::Allow,
                PolicyReasonCodeV1::ContentPartial,
            ),
            ExternalContentStatusV1::TemporarilyUnavailable => (
                SourceAuthorizationDispositionV1::Indeterminate,
                PolicyReasonCodeV1::ContentTemporarilyUnavailable,
            ),
            ExternalContentStatusV1::AuthoritativeDeleted => (
                SourceAuthorizationDispositionV1::Allow,
                PolicyReasonCodeV1::ContentAuthoritativeDeleted,
            ),
        };
        reasons.push(content_reason);
        self.decision(
            input,
            SourceAccessDecisionV1::Authorized,
            coverage,
            disposition,
            Some(effective_grant),
            reasons,
        )
    }
}

/// Apply the non-disclosure boundary after authorization. Reasons, content
/// counts, cursors, source state, and timing never appear in the
/// `NotFoundOrNotAuthorized` variant.
pub fn public_source_result_shape(
    decision: &SourceAuthorizationDecisionV1,
    source_visible: bool,
) -> PublicSourceResultShapeV1 {
    if !source_visible || decision.access == SourceAccessDecisionV1::Unauthorized {
        return PublicSourceResultShapeV1::NotFoundOrNotAuthorized;
    }
    if decision.access == SourceAccessDecisionV1::PolicyExcluded {
        return PublicSourceResultShapeV1::PolicyExcluded;
    }
    if decision.authorization_coverage == AuthorizationCoverageV1::Partial
        || decision.content_status == ExternalContentStatusV1::Partial
    {
        return PublicSourceResultShapeV1::Partial;
    }
    match decision.content_status {
        ExternalContentStatusV1::Live => PublicSourceResultShapeV1::Live,
        ExternalContentStatusV1::Partial => PublicSourceResultShapeV1::Partial,
        ExternalContentStatusV1::TemporarilyUnavailable => {
            PublicSourceResultShapeV1::TemporarilyUnavailable
        }
        ExternalContentStatusV1::AuthoritativeDeleted => {
            PublicSourceResultShapeV1::AuthoritativeDeleted
        }
    }
}
