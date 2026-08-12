//! Invocation-observability test coverage.

use super::*;
use tracedecay_domain::{RepositoryId, WorktreeId};

#[test]
fn feedback_rejection_observation_classifies_request_and_revision_failures() {
    let invalid = DaemonInvocationResponse::problem(
        "request.invalid",
        DaemonInvocationProblem::InvalidRequest,
    );
    assert_eq!(
        invocation_rejected_argument(&invalid),
        Some((
            FeedbackRejectedArgumentV1::RequestBody,
            FeedbackArgumentRejectionClassV1::InvalidShape,
        ))
    );

    let unsupported = DaemonInvocationResponse::problem(
        "request.unsupported",
        DaemonInvocationProblem::UnsupportedRevision,
    );
    assert_eq!(
        invocation_rejected_argument(&unsupported),
        Some((
            FeedbackRejectedArgumentV1::Lifecycle,
            FeedbackArgumentRejectionClassV1::Unsupported,
        ))
    );

    let contract_violation = DaemonInvocationResponse::problem(
        "request.application-contract",
        DaemonInvocationProblem::ApplicationContractViolation,
    );
    assert_eq!(invocation_rejected_argument(&contract_violation), None);
    assert_eq!(
        invocation_response_outcome(&contract_violation),
        FeedbackOutcomeV1::Unavailable
    );
}

#[test]
fn reset_required_observation_is_distinct_from_unavailability() {
    let typed = DaemonInvocationResponse::application_problem(
        "request.typed-reset",
        ApplicationProblem::reset_required(
            SafeDiagnostic::new(
                "configuration.reset_required",
                "The configuration authority requires reset",
            )
            .expect("diagnostic"),
        ),
    );
    assert_eq!(
        invocation_response_outcome(&typed),
        FeedbackOutcomeV1::ResetRequired
    );

    let legacy = DaemonInvocationResponse::problem(
        "request.legacy-reset",
        DaemonInvocationProblem::ResetRequired,
    );
    assert_eq!(
        invocation_response_outcome(&legacy),
        FeedbackOutcomeV1::ResetRequired
    );
}

#[test]
fn scoped_retained_problems_keep_problem_outcome_and_rejection_classification() {
    let scope = ResolvedScope::new(
        ProjectId::new("project.retained.observability").expect("project"),
        RepositoryId::new("repository.retained.observability").expect("repository"),
        WorktreeId::new("worktree.retained.observability").expect("worktree"),
        None,
    )
    .expect("scope");
    let response = DaemonInvocationResponse::retained_application_problem(
        "request.retained.observability",
        scope,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic::new(
                "retained.observability.invalid",
                "The retained observability fixture request is invalid",
            )
            .expect("diagnostic"),
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    );

    assert_eq!(
        invocation_response_outcome(&response),
        FeedbackOutcomeV1::Rejected
    );
    assert_eq!(
        invocation_rejected_argument(&response),
        Some((
            FeedbackRejectedArgumentV1::RequestBody,
            FeedbackArgumentRejectionClassV1::InvalidShape,
        ))
    );
}
