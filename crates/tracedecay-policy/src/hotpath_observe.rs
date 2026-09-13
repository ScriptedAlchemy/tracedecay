//! Opt-in hotpath outcome counters for policy evaluation.
//!
//! Keys are static, bounded outcome classes. Never pass identifiers, digests,
//! reason text, or content. Every call is a no-op unless this crate's
//! `hotpath` feature is selected.

use crate::authorization::{
    SinkRecheckDispositionV1, SourceAccessDecisionV1, SourceAuthorizationDispositionV1,
};
use crate::routing::CapabilityRoutingDispositionV1;

/// One bounded outcome class per source authorization decision. Allowed means
/// authorized access with an allowing disposition; every deny and
/// not-applicable disposition counts as denied; stale, ambiguous, abstaining,
/// and unavailable states count as indeterminate.
#[inline]
pub(crate) fn authorization_outcome(
    access: SourceAccessDecisionV1,
    disposition: SourceAuthorizationDispositionV1,
) {
    match (access, disposition) {
        (SourceAccessDecisionV1::Authorized, SourceAuthorizationDispositionV1::Allow) => {
            hotpath::gauge!("policy.authorization.outcome.allowed").inc(1.0);
        }
        (
            _,
            SourceAuthorizationDispositionV1::Deny
            | SourceAuthorizationDispositionV1::NotApplicable,
        ) => {
            hotpath::gauge!("policy.authorization.outcome.denied").inc(1.0);
        }
        _ => {
            hotpath::gauge!("policy.authorization.outcome.indeterminate").inc(1.0);
        }
    }
}

/// One bounded outcome class per sink admission recheck.
#[inline]
pub(crate) fn recheck_outcome(disposition: SinkRecheckDispositionV1) {
    match disposition {
        SinkRecheckDispositionV1::Admit => {
            hotpath::gauge!("policy.authorization.recheck.admitted").inc(1.0);
        }
        SinkRecheckDispositionV1::Deny => {
            hotpath::gauge!("policy.authorization.recheck.denied").inc(1.0);
        }
        SinkRecheckDispositionV1::Indeterminate => {
            hotpath::gauge!("policy.authorization.recheck.indeterminate").inc(1.0);
        }
    }
}

/// Proof issuance is all-or-nothing; a refusal is recorded, never silent.
#[inline]
pub(crate) fn proof_issuance(issued: bool) {
    if issued {
        hotpath::gauge!("policy.authorization.proof.issued").inc(1.0);
    } else {
        hotpath::gauge!("policy.authorization.proof.refused").inc(1.0);
    }
}

/// One bounded outcome class per capability routing decision, plus the size
/// of the candidate set the evaluation walked.
#[inline]
pub(crate) fn routing_outcome(disposition: CapabilityRoutingDispositionV1, candidates: usize) {
    hotpath::gauge!("policy.routing.candidates").set(candidates as f64);
    match disposition {
        CapabilityRoutingDispositionV1::Allow => {
            hotpath::gauge!("policy.routing.outcome.allowed").inc(1.0);
        }
        CapabilityRoutingDispositionV1::Deny => {
            hotpath::gauge!("policy.routing.outcome.denied").inc(1.0);
        }
        CapabilityRoutingDispositionV1::NotApplicable => {
            hotpath::gauge!("policy.routing.outcome.not_applicable").inc(1.0);
        }
        CapabilityRoutingDispositionV1::Indeterminate => {
            hotpath::gauge!("policy.routing.outcome.indeterminate").inc(1.0);
        }
    }
}
