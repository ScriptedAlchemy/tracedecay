//! Store-boundary Hotpath disposition counters.
//!
//! Duration spans time success and failure alike but cannot distinguish a
//! committed reduction from an exact-duplicate replay or a rejected
//! compare-and-swap, and that split is exactly what a retry-storm diagnosis
//! needs. Keys form a closed static vocabulary; no path, ID, digest, or error
//! content ever becomes a key. Feature-off builds compile every caller
//! identically and record nothing.

use crate::external_source::{
    SourceCommitApplyOutcomeV1, SourceProjectionApplyOutcomeV1, SourceStoreResult,
};
use crate::session::SessionTemporalProjectionBatchDispositionV1;

pub(crate) fn record_source_commit_outcome(
    outcome: &SourceStoreResult<SourceCommitApplyOutcomeV1>,
) {
    #[cfg(feature = "hotpath")]
    match outcome {
        Ok(SourceCommitApplyOutcomeV1::Committed(_)) => {
            hotpath::gauge!("store.external_source.apply_commit.committed").inc(1_u64);
        }
        Ok(SourceCommitApplyOutcomeV1::ExactDuplicate(_)) => {
            hotpath::gauge!("store.external_source.apply_commit.exact_duplicate").inc(1_u64);
        }
        Err(_) => {
            hotpath::gauge!("store.external_source.apply_commit.rejected").inc(1_u64);
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = outcome;
}

pub(crate) fn record_source_projection_outcome(
    outcome: &SourceStoreResult<SourceProjectionApplyOutcomeV1>,
) {
    #[cfg(feature = "hotpath")]
    match outcome {
        Ok(SourceProjectionApplyOutcomeV1::Projected(_)) => {
            hotpath::gauge!("store.external_source.apply_projection.projected").inc(1_u64);
        }
        Ok(SourceProjectionApplyOutcomeV1::ExactDuplicate(_)) => {
            hotpath::gauge!("store.external_source.apply_projection.exact_duplicate").inc(1_u64);
        }
        Err(_) => {
            hotpath::gauge!("store.external_source.apply_projection.rejected").inc(1_u64);
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = outcome;
}

pub(crate) fn record_session_projection_batch_disposition(
    disposition: SessionTemporalProjectionBatchDispositionV1,
) {
    #[cfg(feature = "hotpath")]
    match disposition {
        SessionTemporalProjectionBatchDispositionV1::Applied => {
            hotpath::gauge!("store.session.projection_batch.applied").inc(1_u64);
        }
        SessionTemporalProjectionBatchDispositionV1::ExactReplay => {
            hotpath::gauge!("store.session.projection_batch.exact_replay").inc(1_u64);
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = disposition;
}
