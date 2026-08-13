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
async fn advisory_cycle_is_unavailable_when_no_runtime_owner_is_registered() {
    let response = execute_feedback_advisory_cycle(
        "request.feedback-cycle-unmounted".to_owned(),
        None,
        "file:///project/src/lib.rs".to_owned(),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
    )
    .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::Unavailable {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.advisory-cycle.unavailable"
    ));
}

struct MountedAdvisoryCycle;

impl DaemonAdvisoryCycleInvocationPort for MountedAdvisoryCycle {
    fn invoke(
        &self,
        _request: DaemonAdvisoryCycleInvocationRequest,
    ) -> DaemonAdvisoryCycleInvocationFuture<'_> {
        Box::pin(async {
            Err(ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "feedback.test-mounted-advisory-owner".to_owned(),
                    message: "The mounted advisory owner received the request".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn advisory_cycle_dispatches_to_the_mounted_project_owner() {
    let observed_at = current_micros();
    let project_id = ProjectId::new("project.feedback-cycle-mounted").expect("project id");
    let owner = DaemonAdvisoryCycleInvocationOwner::new(project_id, Arc::new(MountedAdvisoryCycle));
    let response = execute_feedback_advisory_cycle(
        "request.feedback-cycle-mounted".to_owned(),
        Some(owner),
        "file:///project/src/lib.rs".to_owned(),
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle-mounted").expect("cancellation"),
    )
    .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.test-mounted-advisory-owner"
    ));
}
