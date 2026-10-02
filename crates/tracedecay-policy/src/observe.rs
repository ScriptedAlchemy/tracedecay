//! Policy-evaluation outcome gauges.
//!
//! Keys are static, bounded outcome classes. Never pass identifiers, digests,
//! reason text, or content. Every call is a no-op unless this crate's
//! `metrics` recorder is installed.

use crate::routing::CapabilityRoutingDispositionV1;

/// One bounded outcome class per capability routing decision, plus the size
/// of the candidate set the evaluation walked.
#[inline]
pub(crate) fn routing_outcome(disposition: CapabilityRoutingDispositionV1, candidates: usize) {
    metrics::gauge!("policy.routing.candidates").set(candidates as f64);
    match disposition {
        CapabilityRoutingDispositionV1::Allow => {
            metrics::gauge!("policy.routing.outcome.allowed").increment(1.0);
        }
        CapabilityRoutingDispositionV1::Deny => {
            metrics::gauge!("policy.routing.outcome.denied").increment(1.0);
        }
        CapabilityRoutingDispositionV1::NotApplicable => {
            metrics::gauge!("policy.routing.outcome.not_applicable").increment(1.0);
        }
        CapabilityRoutingDispositionV1::Indeterminate => {
            metrics::gauge!("policy.routing.outcome.indeterminate").increment(1.0);
        }
    }
}
