use crate::result::{ApplicationProblem, ApplicationProblemKind};
use tracedecay_domain::feedback::{FeedbackCycleTerminationV1, ProviderEvaluationStateV1};

/// Project an application problem into the feedback cycle's terminal state.
/// Admitted partial effects and reset-required states remain distinct from a
/// daemon outage so their terminal evidence is not lost at the feedback seam.
pub(super) fn terminal_for_problem(
    problem: &ApplicationProblem,
) -> (FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>) {
    terminal_for_problem_kind(problem.kind())
}

fn terminal_for_problem_kind(
    kind: ApplicationProblemKind,
) -> (FeedbackCycleTerminationV1, Vec<ProviderEvaluationStateV1>) {
    match kind {
        ApplicationProblemKind::Cancelled => (
            FeedbackCycleTerminationV1::Cancelled,
            vec![ProviderEvaluationStateV1::Cancelled],
        ),
        ApplicationProblemKind::TimedOut => (
            FeedbackCycleTerminationV1::BudgetExceeded,
            vec![ProviderEvaluationStateV1::TimedOut],
        ),
        ApplicationProblemKind::Stale => (
            FeedbackCycleTerminationV1::StaleReplanRequired,
            vec![ProviderEvaluationStateV1::Stale],
        ),
        ApplicationProblemKind::Unavailable => (
            FeedbackCycleTerminationV1::DaemonUnavailable,
            vec![ProviderEvaluationStateV1::Unavailable],
        ),
        ApplicationProblemKind::PartialEffect => (
            FeedbackCycleTerminationV1::IncompleteCoverage,
            vec![ProviderEvaluationStateV1::Partial],
        ),
        ApplicationProblemKind::ResetRequired => (FeedbackCycleTerminationV1::Blocked, Vec::new()),
        ApplicationProblemKind::InvalidRequest
        | ApplicationProblemKind::NotFoundOrNotAuthorized
        | ApplicationProblemKind::Conflict
        | ApplicationProblemKind::Unsupported
        | ApplicationProblemKind::Saturated => (FeedbackCycleTerminationV1::Blocked, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_problem_mapping_preserves_admitted_partial_and_required_reset() {
        assert_eq!(
            terminal_for_problem_kind(ApplicationProblemKind::PartialEffect),
            (
                FeedbackCycleTerminationV1::IncompleteCoverage,
                vec![ProviderEvaluationStateV1::Partial],
            )
        );
        assert_eq!(
            terminal_for_problem_kind(ApplicationProblemKind::ResetRequired),
            (FeedbackCycleTerminationV1::Blocked, Vec::new())
        );
    }
}
