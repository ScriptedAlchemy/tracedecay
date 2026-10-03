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
        Ok(SourceCommitApplyOutcomeV1::Committed(_)) => {
            metrics::gauge!("store.external_source.apply_commit.committed").increment(1.0);
        }
        Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(_)) => {
            metrics::gauge!("store.external_source.apply_commit.exact_duplicate").increment(1.0);
        }
        Err(_) => {
            metrics::gauge!("store.external_source.apply_commit.rejected").increment(1.0);
        }
    }
}

pub(crate) fn record_source_projection_outcome(
    outcome: &SourceStoreResult<SourceProjectionApplyOutcomeV1>,
) {
    match outcome {
        Ok(SourceProjectionApplyOutcomeV1::Projected(_)) => {
            metrics::gauge!("store.external_source.apply_projection.projected").increment(1.0);
        }
        Ok(SourceProjectionApplyOutcomeV1::ExactDuplicate(_)) => {
            metrics::gauge!("store.external_source.apply_projection.exact_duplicate")
                .increment(1.0);
        }
        Err(_) => {
            metrics::gauge!("store.external_source.apply_projection.rejected").increment(1.0);
        }
    }
}

pub(crate) fn record_session_projection_batch_disposition(
    disposition: SessionTemporalProjectionBatchDispositionV1,
) {
    match disposition {
        SessionTemporalProjectionBatchDispositionV1::Applied => {
            metrics::gauge!("store.session.projection_batch.applied").increment(1.0);
        }
        SessionTemporalProjectionBatchDispositionV1::ExactReplay => {
            metrics::gauge!("store.session.projection_batch.exact_replay").increment(1.0);
        }
    }
}
