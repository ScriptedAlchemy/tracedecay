//! `feedback` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[test]
fn advisory_cycle_wire_request_has_no_client_selected_handle() {
    let request = DaemonInvocationRequest::feedback_advisory_cycle(
        "request.feedback-cycle",
        "file:///project/src/lib.rs".to_owned(),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
    );
    let value = serde_json::to_value(request).expect("wire request");

    assert_eq!(value["operation"], "feedback_advisory_cycle");
    assert_eq!(value["document_uri"], "file:///project/src/lib.rs");
    assert!(value.get("request_handle").is_none());
}

#[tokio::test]
async fn advisory_cycle_refuses_when_no_effect_authority_is_registered() {
    let response =
        execute_feedback_advisory_cycle("request.feedback-cycle-unmounted".to_owned()).await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::Unavailable {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.advisory_cycle_quarantined"
    ));
}
