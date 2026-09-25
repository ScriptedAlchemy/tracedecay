//! Opt-in hotpath outcome counters for policy evaluation.
//!
//! Keys are static, bounded outcome classes. Never pass identifiers, digests,
//! reason text, or content. Every call is a no-op unless this crate's
//! `hotpath` feature is selected.

use crate::routing::CapabilityRoutingDispositionV1;

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
