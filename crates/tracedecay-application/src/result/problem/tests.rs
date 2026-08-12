use super::{
    ApplicationExecutionFailureClassV1, ApplicationProblem, ApplicationProblemKind,
    ApplicationUnavailableClassV1, CancellationStage, LegalAction, ProblemTerminality,
    RetryDirective, SafeDiagnostic,
};

#[test]
fn reset_required_is_a_distinct_non_retryable_terminal() {
    let problem = ApplicationProblem::reset_required(
        SafeDiagnostic::new("store.reset_required", "The store must be reset.")
            .expect("fixture diagnostic is valid"),
    );

    assert_eq!(problem.kind(), ApplicationProblemKind::ResetRequired);
    assert_eq!(problem.canonical_code(), "reset_required");
    assert_eq!(problem.retry(), RetryDirective::Never);
    assert_eq!(problem.legal_actions(), &[LegalAction::Reset]);
    assert_eq!(problem.terminality(), ProblemTerminality::AdmittedTerminal);
    assert!(problem.is_admitted_terminal());
    assert!(problem.committed_receipt().is_none());

    let wire = serde_json::to_value(&problem).expect("problem serializes");
    assert_eq!(wire["kind"], "reset_required");
    assert_eq!(wire["retry"], "never");
    assert_eq!(wire["legal_actions"], serde_json::json!(["reset"]));

    let mut unknown = wire.clone();
    unknown["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ApplicationProblem>(unknown).is_err());

    let mut retrying = wire.clone();
    retrying["retry"] = serde_json::json!("after_delay");
    assert!(serde_json::from_value::<ApplicationProblem>(retrying).is_err());

    let mut wrong_action = wire;
    wrong_action["legal_actions"] = serde_json::json!(["retry"]);
    assert!(serde_json::from_value::<ApplicationProblem>(wrong_action).is_err());
}

#[test]
fn cancellation_terminality_is_bound_to_the_exact_observed_stage() {
    let pre_admission = ApplicationProblem::cancelled_before_admission();
    assert_eq!(
        pre_admission.terminality(),
        ProblemTerminality::PreAdmission
    );
    assert_eq!(
        pre_admission.cancellation_stage(),
        Some(CancellationStage::BeforeAdmission)
    );

    for stage in [
        CancellationStage::BeforeRead,
        CancellationStage::DuringRead,
        CancellationStage::BeforeEffect,
        CancellationStage::EffectInFlight,
    ] {
        let cancelled =
            ApplicationProblem::cancelled(stage).expect("admitted cancellation stage is valid");
        let timed_out =
            ApplicationProblem::timed_out(stage).expect("admitted timeout stage is valid");
        for terminal in [cancelled, timed_out] {
            assert_eq!(terminal.terminality(), ProblemTerminality::AdmittedTerminal);
            assert_eq!(terminal.cancellation_stage(), Some(stage));
            let wire = serde_json::to_value(&terminal).expect("terminal serializes");
            assert_eq!(wire["stage"], serde_json::to_value(stage).expect("stage"));
            assert_eq!(
                serde_json::from_value::<ApplicationProblem>(wire).expect("terminal round trips"),
                terminal
            );
        }
    }
}

#[test]
fn cancellation_after_effect_or_during_reconciliation_is_not_a_no_effect_terminal() {
    for stage in [
        CancellationStage::Reconciling,
        CancellationStage::AfterCommit,
    ] {
        assert!(ApplicationProblem::cancelled(stage).is_err());
        assert!(ApplicationProblem::timed_out(stage).is_err());
        let wire = serde_json::json!({
            "kind": "cancelled", "stage": stage, "retry": "never", "legal_actions": []
        });
        assert!(serde_json::from_value::<ApplicationProblem>(wire).is_err());
    }
}

#[test]
fn unavailable_and_execution_failure_classes_cannot_change_admission_semantics() {
    let authority = ApplicationProblem::unavailable(
        SafeDiagnostic::new("authority.unavailable", "The authority is unavailable")
            .expect("diagnostic"),
    );
    assert_eq!(authority.terminality(), ProblemTerminality::PreAdmission);
    assert_eq!(
        authority.unavailable_classification(),
        Some(ApplicationUnavailableClassV1::Authority)
    );

    for classification in [
        ApplicationUnavailableClassV1::BackendUnavailable,
        ApplicationUnavailableClassV1::BackendDisconnected,
        ApplicationUnavailableClassV1::BackendRetryable,
    ] {
        let terminal = ApplicationProblem::admitted_unavailable(
            classification,
            SafeDiagnostic::new("backend.unavailable", "The backend is unavailable")
                .expect("diagnostic"),
        )
        .expect("admitted unavailable terminal");
        assert_eq!(terminal.terminality(), ProblemTerminality::AdmittedTerminal);
        assert_eq!(terminal.unavailable_classification(), Some(classification));
        let wire = serde_json::to_value(&terminal).expect("terminal serializes");
        assert_eq!(
            serde_json::from_value::<ApplicationProblem>(wire).expect("terminal decodes"),
            terminal
        );
    }

    for classification in [
        ApplicationExecutionFailureClassV1::Denied,
        ApplicationExecutionFailureClassV1::MalformedOutput,
        ApplicationExecutionFailureClassV1::Permanent,
    ] {
        let terminal = ApplicationProblem::execution_failed(
            classification,
            SafeDiagnostic::new("backend.failed", "The backend execution failed")
                .expect("diagnostic"),
        )
        .expect("execution-failed terminal");
        assert_eq!(terminal.terminality(), ProblemTerminality::AdmittedTerminal);
        assert_eq!(
            terminal.execution_failure_classification(),
            Some(classification)
        );
    }

    let mut authority_wire = serde_json::to_value(authority).expect("authority wire");
    authority_wire["classification"] = serde_json::json!("backend_unavailable");
    authority_wire["retry"] = serde_json::json!("after_delay");
    assert!(serde_json::from_value::<ApplicationProblem>(authority_wire).is_err());
}

#[test]
fn direct_serialization_rejects_an_invalid_terminal() {
    let invalid = ApplicationProblem::ResetRequired {
        diagnostic: SafeDiagnostic::new("store.reset_required", "The store must be reset.")
            .expect("fixture diagnostic is valid"),
        retry: RetryDirective::AfterDelay,
        legal_actions: vec![LegalAction::Retry],
    };
    assert!(serde_json::to_value(invalid).is_err());
}
