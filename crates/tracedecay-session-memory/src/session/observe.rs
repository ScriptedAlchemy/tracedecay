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
        SessionRetrievalBudgetStageV1::RequestResultLimit => {
            metrics::gauge!("session.retrieval.budget.request_results").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestHydrationLimit => {
            metrics::gauge!("session.retrieval.budget.request_hydration_items").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestContextBytes => {
            metrics::gauge!("session.retrieval.budget.request_context_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestCandidateBytes => {
            metrics::gauge!("session.retrieval.budget.request_candidate_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestRecordBytes => {
            metrics::gauge!("session.retrieval.budget.request_record_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RequestHydrationBytes => {
            metrics::gauge!("session.retrieval.budget.request_hydration_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::EstimatorVersionMismatch => {
            metrics::gauge!("session.retrieval.budget.estimator_version").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::ExecutionWorkExhausted => {
            metrics::gauge!("session.retrieval.budget.execution_work").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::CandidateReadExhausted => {
            metrics::gauge!("session.retrieval.budget.candidate_read").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::RecordReadExhausted => {
            metrics::gauge!("session.retrieval.budget.record_read").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::KernelResultLimit => {
            metrics::gauge!("session.retrieval.budget.kernel_results").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::CursorManifestLimit => {
            metrics::gauge!("session.retrieval.budget.cursor_manifest").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::ParticipantManifestParticipants => {
            metrics::gauge!("session.retrieval.budget.manifest_participants").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::ParticipantManifestCanonicalBytes => {
            metrics::gauge!("session.retrieval.budget.manifest_canonical_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::HydrationBytes => {
            metrics::gauge!("session.retrieval.budget.hydration_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::ContextBytes => {
            metrics::gauge!("session.retrieval.budget.context_bytes").increment(1.0);
        }
        SessionRetrievalBudgetStageV1::ContextTokens => {
            metrics::gauge!("session.retrieval.budget.context_tokens").increment(1.0);
        }
    }
}
