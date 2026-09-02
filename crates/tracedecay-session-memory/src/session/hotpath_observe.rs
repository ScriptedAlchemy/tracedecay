//! Opt-in hotpath gauges owned by session retrieval.
//!
//! Keys are static capability names. Never pass model inputs, paths, or
//! generation identifiers as labels. Every macro expands to a no-op unless
//! this crate's `hotpath` feature is selected.

use tracedecay_application::retrieval::SessionRetrievalBudgetStageV1;

/// Count one bounded session-retrieval budget stage. Keys stay static; the
/// stage is never a dynamic label.
#[inline]
pub(crate) fn session_retrieval_budget_stage(stage: SessionRetrievalBudgetStageV1) {
    match stage {
        SessionRetrievalBudgetStageV1::RequestResultLimit => {
            hotpath::gauge!("session.retrieval.budget.request_results").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestHydrationLimit => {
            hotpath::gauge!("session.retrieval.budget.request_hydration_items").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestContextBytes => {
            hotpath::gauge!("session.retrieval.budget.request_context_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestCandidateBytes => {
            hotpath::gauge!("session.retrieval.budget.request_candidate_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestRecordBytes => {
            hotpath::gauge!("session.retrieval.budget.request_record_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestHydrationBytes => {
            hotpath::gauge!("session.retrieval.budget.request_hydration_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::EstimatorVersionMismatch => {
            hotpath::gauge!("session.retrieval.budget.estimator_version").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::ExecutionWorkExhausted => {
            hotpath::gauge!("session.retrieval.budget.execution_work").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::KernelResultLimit => {
            hotpath::gauge!("session.retrieval.budget.kernel_results").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::ParticipantManifestParticipants => {
            hotpath::gauge!("session.retrieval.budget.manifest_participants").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::ParticipantManifestCanonicalBytes => {
            hotpath::gauge!("session.retrieval.budget.manifest_canonical_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::HydrationBytes => {
            hotpath::gauge!("session.retrieval.budget.hydration_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::ContextBytes => {
            hotpath::gauge!("session.retrieval.budget.context_bytes").inc(1.0);
        }
        SessionRetrievalBudgetStageV1::ContextTokens => {
            hotpath::gauge!("session.retrieval.budget.context_tokens").inc(1.0);
        }
    }
}
