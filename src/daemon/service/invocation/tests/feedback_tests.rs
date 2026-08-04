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

#[test]
fn cancelled_advisory_cycle_preserves_a_typed_execution_receipt() {
    let observed_at = current_micros();
    let project_id = ProjectId::new("project.feedback-advisory").expect("project id");
    let scope = ResolvedScope::new(
        project_id,
        tracedecay_domain::RepositoryId::new("repository.feedback-advisory")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.feedback-advisory").expect("worktree id"),
        None,
    )
    .expect("scope");
    let actor = ActorId::new("actor.feedback-advisory").expect("actor");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.feedback-advisory").expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        actor.clone(),
        UtcMicros(observed_at.0.saturating_sub(1)),
        UtcMicros(observed_at.0.saturating_add(60_000_000)),
        scope.clone(),
        std::collections::BTreeSet::from([CapabilityId::new(
            "capability.application.feedback.advisory-cycle",
        )
        .expect("capability")]),
        std::collections::BTreeSet::from([UseCaseId::new(
            "use-case.application.feedback.advisory-cycle",
        )
        .expect("use case")]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    let deadline =
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline");
    let cancelled = CancellationContext::cancelled(
        "cancel.feedback-advisory",
        UtcMicros(observed_at.0.saturating_add(1)),
    )
    .expect("cancellation");
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.feedback-advisory").expect("request id"),
        deadline.clone(),
        cancelled.clone(),
    )
    .expect("context");

    let result = advisory_cycle_invocation_result(
        &context,
        observed_at,
        deadline,
        cancelled,
        AdvisoryCycleOutcome::Cancelled {
            contributions: crate::application::advisory::Pr13AdvisoryContributionsV1::absent(),
        },
    )
    .expect("typed advisory result");

    assert_eq!(
        result.evidence.execution.termination,
        OperationTermination::Cancelled
    );
    assert_eq!(
        result
            .evidence
            .execution
            .cancellation
            .as_ref()
            .map(|observation| observation.stage),
        Some(CancellationStage::DuringRead)
    );
    assert!(result.evidence.payload.is_none());
}
