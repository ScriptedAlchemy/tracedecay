use super::*;

#[test]
fn stale_qualification_keeps_its_machine_reason_across_daemon_boundary() {
    let response = semantic_evaluation_response(
        "req-stale-qualification".to_owned(),
        Err(
            tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                SemanticActivationCoordinationErrorV1::Qualification(
                    tracedecay_contracts::SemanticQualificationFailureV1::StaleWorkload {
                        profile_id: "hybrid-conservative".to_owned(),
                        packaged_workload_digest: "sha256:old".to_owned(),
                        current_workload_digest: "sha256:current".to_owned(),
                        remedy: "run qualify-native".to_owned(),
                    },
                ),
            ),
        ),
    );

    let DaemonInvocationOutcome::ApplicationProblem { problem } = response.outcome else {
        panic!("expected typed qualification refusal");
    };
    let diagnostic = problem.diagnostic().expect("qualification diagnostic");
    assert_eq!(diagnostic.code, "semantic_qualification.stale_workload");
    assert!(diagnostic.message.contains("sha256:old"));
    assert!(diagnostic.message.contains("sha256:current"));
    assert!(diagnostic.message.contains("qualify-native"));
}
