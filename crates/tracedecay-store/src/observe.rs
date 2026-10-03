//! Store-boundary disposition gauges.
//!
//! Duration spans time success and failure alike but cannot distinguish a
//! committed reduction from an exact-duplicate replay or a rejected
//! compare-and-swap, and that split is exactly what a retry-storm diagnosis
//! needs. Keys form a closed static vocabulary; no path, ID, digest, or error
//! content ever becomes a key. Every update goes through the `metrics` facade
//! and is dropped when no recorder is installed.

use crate::external_source::{
    SourceCommitApplyOutcomeV1, SourceProjectionApplyOutcomeV1, SourceStoreResult,
};
use crate::session::SessionTemporalProjectionBatchDispositionV1;

pub(crate) fn record_source_commit_outcome(
    outcome: &SourceStoreResult<SourceCommitApplyOutcomeV1>,
) {
    match outcome {
        Ok(SourceCommitApplyOutcomeV1::Committed(_)) => {}
        Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(_)) => {}
        Err(_) => {}
    }
}

pub(crate) fn record_source_projection_outcome(
    outcome: &SourceStoreResult<SourceProjectionApplyOutcomeV1>,
) {
    match outcome {
        Ok(SourceProjectionApplyOutcomeV1::Projected(_)) => {}
        Ok(SourceProjectionApplyOutcomeV1::ExactDuplicate(_)) => {}
        Err(_) => {}
    }
}

pub(crate) fn record_session_projection_batch_disposition(
    disposition: SessionTemporalProjectionBatchDispositionV1,
) {
    match disposition {
        SessionTemporalProjectionBatchDispositionV1::Applied => {}
        SessionTemporalProjectionBatchDispositionV1::ExactReplay => {}
    }
}
