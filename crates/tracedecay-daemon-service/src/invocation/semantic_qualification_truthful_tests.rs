use super::*;

#[test]
fn stale_qualification_keeps_its_machine_reason_across_daemon_boundary() {
    let response = semantic_evaluation_response(
        "req-stale-qualification".to_owned(),
        Err(
            tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                SemanticActivationCoordinationErrorV1::Qualification(Box::new(
                    tracedecay_contracts::SemanticQualificationFailureV1::StaleWorkload {
                        profile_id: "hybrid-conservative".to_owned(),
                        packaged_workload_digest: "sha256:old".to_owned(),
                        current_workload_digest: "sha256:current".to_owned(),
                        evidence_digest: "sha256:evidence".to_owned(),
                        remedy: "run qualify-native".to_owned(),
                    },
                )),
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

/// Evidence written under a superseded decision rule crosses the boundary as
/// its own machine reason. Collapsing it into stale workload or absent evidence
/// would send an operator to re-check digests that are in fact intact.
#[test]
fn superseded_decision_rule_keeps_its_own_machine_reason() {
    for (failure, code, expected_versions) in [
        (
            tracedecay_contracts::SemanticQualificationFailureV1::SupersededSchema {
                profile_id: "hybrid-conservative".to_owned(),
                packaged_schema_version: 1,
                current_schema_version: 2,
                evidence_digest: Some("sha256:evidence".to_owned()),
                remedy: "run qualify-native".to_owned(),
            },
            "semantic_qualification.superseded_schema",
            ["schema 1", "schema 2"],
        ),
        (
            tracedecay_contracts::SemanticQualificationFailureV1::SupersededMethodology {
                profile_id: "hybrid-conservative".to_owned(),
                packaged_methodology_version: 1,
                current_methodology_version: 2,
                evidence_digest: Some("sha256:evidence".to_owned()),
                remedy: "run qualify-native".to_owned(),
            },
            "semantic_qualification.superseded_methodology",
            ["methodology 1", "methodology 2"],
        ),
    ] {
        let response = semantic_evaluation_response(
            "req-superseded-qualification".to_owned(),
            Err(
                tracedecay_code_index_runtime::semantic_evaluation::DaemonSemanticEvaluationExecutionErrorV1::Coordination(
                    SemanticActivationCoordinationErrorV1::Qualification(Box::new(failure)),
                ),
            ),
        );

        let DaemonInvocationOutcome::ApplicationProblem { problem } = response.outcome else {
            panic!("expected typed qualification refusal for {code}");
        };
        let diagnostic = problem.diagnostic().expect("qualification diagnostic");
        assert_eq!(diagnostic.code, code);
        for version in expected_versions {
            assert!(
                diagnostic.message.contains(version),
                "{code} must name {version}: {}",
                diagnostic.message
            );
        }
        assert!(diagnostic.message.contains("qualify-native"));
    }
}
