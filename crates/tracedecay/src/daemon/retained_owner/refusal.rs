use tracedecay_application::RetainedSurfaceExecutionErrorV1;
use tracedecay_usecases::session::SessionRetrievalBudgetStageV1;

use crate::daemon::session_retrieval::SessionRetrievalServiceOutcome;

pub(super) fn from_session_retrieval(
    outcome: SessionRetrievalServiceOutcome,
) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { kind, .. } => {
            RetainedSurfaceExecutionErrorV1::cursor_manifest_refusal(kind)
        }
        SessionRetrievalServiceOutcome::BudgetExhausted { stage } => match stage {
            SessionRetrievalBudgetStageV1::RequestBudgetMismatch => {
                RetainedSurfaceExecutionErrorV1::request_budget_mismatch_refusal()
            }
            SessionRetrievalBudgetStageV1::ExecutionWorkExhausted => {
                RetainedSurfaceExecutionErrorV1::execution_work_exhausted_refusal()
            }
            SessionRetrievalBudgetStageV1::ParticipantManifestLimit => {
                RetainedSurfaceExecutionErrorV1::participant_manifest_budget_refusal()
            }
            SessionRetrievalBudgetStageV1::HydrationBytes => {
                RetainedSurfaceExecutionErrorV1::hydration_bytes_budget_refusal()
            }
            SessionRetrievalBudgetStageV1::ContextBytes => {
                RetainedSurfaceExecutionErrorV1::context_bytes_budget_refusal()
            }
        },
        SessionRetrievalServiceOutcome::Complete { .. }
        | SessionRetrievalServiceOutcome::CompleteZero { .. }
        | SessionRetrievalServiceOutcome::Stale { .. }
        | SessionRetrievalServiceOutcome::Partial { .. }
        | SessionRetrievalServiceOutcome::WrongScope
        | SessionRetrievalServiceOutcome::Locked
        | SessionRetrievalServiceOutcome::Redacted
        | SessionRetrievalServiceOutcome::Deleted
        | SessionRetrievalServiceOutcome::Denied
        | SessionRetrievalServiceOutcome::ResetRequired { .. }
        | SessionRetrievalServiceOutcome::Unavailable(_)
        | SessionRetrievalServiceOutcome::Cancelled => {
            RetainedSurfaceExecutionErrorV1::structural_budget_refusal()
        }
    }
}
