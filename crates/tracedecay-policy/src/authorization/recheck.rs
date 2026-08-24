use serde::Serialize;
use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::decision::{
    PolicyReasonCodeV1, SourceAuthorizationDecisionV1, SourceAuthorizationEvaluator,
};
use super::input::{
    ExternalContentStatusV1, SourceAuthorizationInputV1, TypedOperationV1, policy_digest,
};
use super::intersection::EffectiveSourceGrantV1;
use super::state::SourceAuthorizationDispositionV1;

/// Opaque proof emitted only from an allow decision. Its fields are private so
/// callers cannot manufacture a transition around the evaluator.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceAuthorizationProofV1 {
    input_digest: ManifestDigest,
    authority_fingerprint: ManifestDigest,
    decision_digest: ManifestDigest,
    effective_grant: EffectiveSourceGrantV1,
    source_grant_expires_at: UtcMicros,
    requester_grant_expires_at: UtcMicros,
    sink_policy_revision: u64,
    sink_policy_digest: ManifestDigest,
}

impl SourceAuthorizationProofV1 {
    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub fn authority_fingerprint(&self) -> &ManifestDigest {
        &self.authority_fingerprint
    }

    pub fn effective_grant(&self) -> &EffectiveSourceGrantV1 {
        &self.effective_grant
    }

    fn expires_at(&self) -> UtcMicros {
        self.source_grant_expires_at
            .min(self.requester_grant_expires_at)
    }
}

/// Opaque admission proof required by effect-owning application code. It
/// proves fresh recheck only; it does not execute or authorize a side effect.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SinkAdmissionProofV1 {
    proof_digest: ManifestDigest,
    authority_fingerprint: ManifestDigest,
    effective_grant: EffectiveSourceGrantV1,
    admitted_at: UtcMicros,
    expires_at: UtcMicros,
}

impl SinkAdmissionProofV1 {
    pub fn proof_digest(&self) -> &ManifestDigest {
        &self.proof_digest
    }

    pub fn effective_grant(&self) -> &EffectiveSourceGrantV1 {
        &self.effective_grant
    }

    pub fn expires_at(&self) -> UtcMicros {
        self.expires_at
    }
}

/// Sink recheck is intentionally separate from source authorization: an old
/// allow cannot be reused after grants, binding, owner, policy, configuration,
/// privacy, or sink state drift.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SinkRecheckDispositionV1 {
    Admit,
    Deny,
    Indeterminate,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SinkRecheckDecisionV1 {
    pub disposition: SinkRecheckDispositionV1,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
    admission_proof: Option<SinkAdmissionProofV1>,
}

impl SinkRecheckDecisionV1 {
    pub fn admission_proof(&self) -> Option<&SinkAdmissionProofV1> {
        self.admission_proof.as_ref()
    }
}

/// Issue a source proof from a current, fully allowing decision. Indeterminate
/// availability, partial input, denied access, and policy exclusion cannot
/// transition into a proof.
pub fn issue_source_authorization_proof(
    evaluator: &impl SourceAuthorizationEvaluator,
    input: &SourceAuthorizationInputV1,
    decision: &SourceAuthorizationDecisionV1,
) -> Option<SourceAuthorizationProofV1> {
    if evaluator.evaluate(input) != *decision {
        return None;
    }
    let effective_grant = decision.effective_grant.clone()?;
    if !decision.is_authorized()
        || decision.disposition != SourceAuthorizationDispositionV1::Allow
        || !effective_grant.permits_requested_access(input)
        || decision.input_digest != input.input_digest()
        || (input.content_status == ExternalContentStatusV1::AuthoritativeDeleted
            && input.requested_access.operation != TypedOperationV1::HistoricalRead)
    {
        return None;
    }
    Some(SourceAuthorizationProofV1 {
        input_digest: decision.input_digest.clone(),
        authority_fingerprint: input.authority_fingerprint(),
        decision_digest: decision.decision_digest.clone(),
        effective_grant,
        source_grant_expires_at: input.source_grant.expires_at,
        requester_grant_expires_at: input.requester_grant.expires_at,
        sink_policy_revision: input.sink_policy.policy_revision,
        sink_policy_digest: input.sink_policy.policy_digest.clone(),
    })
}

/// Re-run authorization against current immutable facts immediately before an
/// application sink. No proof survives a revision or privacy drift.
pub fn recheck_sink_admission(
    evaluator: &impl SourceAuthorizationEvaluator,
    proof: &SourceAuthorizationProofV1,
    current: &SourceAuthorizationInputV1,
) -> SinkRecheckDecisionV1 {
    let fresh = evaluator.evaluate(current);
    if !fresh.is_authorized() || fresh.disposition != SourceAuthorizationDispositionV1::Allow {
        return SinkRecheckDecisionV1 {
            disposition: match fresh.disposition {
                SourceAuthorizationDispositionV1::Indeterminate
                | SourceAuthorizationDispositionV1::Abstain => {
                    SinkRecheckDispositionV1::Indeterminate
                }
                SourceAuthorizationDispositionV1::Allow
                | SourceAuthorizationDispositionV1::Deny
                | SourceAuthorizationDispositionV1::NotApplicable => SinkRecheckDispositionV1::Deny,
            },
            ordered_reason_codes: fresh.ordered_reason_codes,
            admission_proof: None,
        };
    }
    if current.content_status == ExternalContentStatusV1::AuthoritativeDeleted
        && current.requested_access.operation != TypedOperationV1::HistoricalRead
    {
        return SinkRecheckDecisionV1 {
            disposition: SinkRecheckDispositionV1::Deny,
            ordered_reason_codes: vec![PolicyReasonCodeV1::AuthorizationInputDrift],
            admission_proof: None,
        };
    }
    if current.evaluated_at >= proof.expires_at() {
        return SinkRecheckDecisionV1 {
            disposition: SinkRecheckDispositionV1::Deny,
            ordered_reason_codes: vec![PolicyReasonCodeV1::AuthorizationInputDrift],
            admission_proof: None,
        };
    }
    if current.sink_policy.policy_revision != proof.sink_policy_revision
        || current.sink_policy.policy_digest != proof.sink_policy_digest
    {
        return SinkRecheckDecisionV1 {
            disposition: SinkRecheckDispositionV1::Deny,
            ordered_reason_codes: vec![PolicyReasonCodeV1::SinkPolicyDrift],
            admission_proof: None,
        };
    }
    let authority_fingerprint = current.authority_fingerprint();
    if authority_fingerprint != proof.authority_fingerprint {
        return SinkRecheckDecisionV1 {
            disposition: SinkRecheckDispositionV1::Deny,
            ordered_reason_codes: vec![PolicyReasonCodeV1::AuthorizationInputDrift],
            admission_proof: None,
        };
    }
    let proof_digest = policy_digest(
        "tracedecay.policy.sink-admission-proof.v1",
        &(
            &proof.input_digest,
            &proof.decision_digest,
            &fresh.decision_digest,
            &authority_fingerprint,
            current.evaluated_at,
        ),
    );
    SinkRecheckDecisionV1 {
        disposition: SinkRecheckDispositionV1::Admit,
        ordered_reason_codes: fresh.ordered_reason_codes,
        admission_proof: Some(SinkAdmissionProofV1 {
            proof_digest,
            authority_fingerprint,
            effective_grant: proof.effective_grant.clone(),
            admitted_at: current.evaluated_at,
            expires_at: proof.expires_at(),
        }),
    }
}
