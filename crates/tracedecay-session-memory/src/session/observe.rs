//! Metrics gauges owned by session retrieval.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! the `metrics` recorder is installed.

use tracedecay_contracts::retrieval::SessionRetrievalBudgetStageV1;

/// Count one bounded session-retrieval budget stage. Keys stay static; the
/// stage is never a dynamic label.
#[inline]
pub(crate) fn session_retrieval_budget_stage(stage: SessionRetrievalBudgetStageV1) {
    match stage {
        SessionRetrievalBudgetStageV1::RequestResultLimit => {}
        SessionRetrievalBudgetStageV1::RequestHydrationLimit => {}
        SessionRetrievalBudgetStageV1::RequestContextBytes => {}
        SessionRetrievalBudgetStageV1::RequestCandidateBytes => {}
        SessionRetrievalBudgetStageV1::RequestRecordBytes => {}
        SessionRetrievalBudgetStageV1::RequestHydrationBytes => {}
        SessionRetrievalBudgetStageV1::EstimatorVersionMismatch => {}
        SessionRetrievalBudgetStageV1::ExecutionWorkExhausted => {}
        SessionRetrievalBudgetStageV1::CandidateReadExhausted => {}
        SessionRetrievalBudgetStageV1::RecordReadExhausted => {}
        SessionRetrievalBudgetStageV1::KernelResultLimit => {}
        SessionRetrievalBudgetStageV1::CursorManifestLimit => {}
        SessionRetrievalBudgetStageV1::ParticipantManifestParticipants => {}
        SessionRetrievalBudgetStageV1::ParticipantManifestCanonicalBytes => {}
        SessionRetrievalBudgetStageV1::HydrationBytes => {}
        SessionRetrievalBudgetStageV1::ContextBytes => {}
        SessionRetrievalBudgetStageV1::ContextTokens => {}
    }
}
