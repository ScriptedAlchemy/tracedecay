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
    match disposition {
        CapabilityRoutingDispositionV1::Allow => {}
        CapabilityRoutingDispositionV1::Deny => {}
        CapabilityRoutingDispositionV1::NotApplicable => {}
        CapabilityRoutingDispositionV1::Indeterminate => {}
    }
}
