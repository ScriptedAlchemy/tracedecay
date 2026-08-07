//! Invocation-observability test coverage.

use super::*;

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
}
