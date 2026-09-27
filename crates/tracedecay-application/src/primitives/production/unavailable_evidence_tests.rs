use super::super::runtime::QualifiedNamePrimitiveResult;
use super::*;
use tracedecay_contracts::ApplicationProblemKind;

#[test]
fn graph_admission_failures_refuse_with_the_typed_graph_state() {
    for (error, reason, refusal) in [
        (
            CodeGraphReadError::Unavailable {
                detail: "the verified graph is not ready".to_owned(),
            },
            OmissionReason::Unavailable,
            Some((
                ApplicationProblemKind::Unavailable,
                "application.code-graph.unavailable",
            )),
        ),
        (
            CodeGraphReadError::Stale {
                detail: "the graph no longer matches the request".to_owned(),
            },
            OmissionReason::Stale,
            Some((
                ApplicationProblemKind::Stale,
                "application.code-graph.stale",
            )),
        ),
        (
            CodeGraphReadError::Cancelled,
            OmissionReason::Cancelled,
            None,
        ),
        (CodeGraphReadError::TimedOut, OmissionReason::TimedOut, None),
    ] {
        let outcome: RetrievalPortOutcome<QualifiedNamePrimitiveResult> =
            graph_read_outcome(&error, EvidenceDomain::Symbol, UtcMicros(1));
        match (&outcome, refusal) {
            (RetrievalPortOutcome::Refused(_, problem), Some((kind, code))) => {
                assert_eq!(
                    (problem.kind(), problem.reason_code()),
                    (kind, code),
                    "{error:?}"
                );
            }
            (RetrievalPortOutcome::Cancelled(_), None) => {
                assert_eq!(reason, OmissionReason::Cancelled);
            }
            (RetrievalPortOutcome::TimedOut(_), None) => {
                assert_eq!(reason, OmissionReason::TimedOut);
            }
            (outcome, _) => panic!("{error:?} produced {outcome:?}"),
        }
        let evidence = outcome.evidence();
        assert!(evidence.payload.is_none());
        assert_eq!(
            evidence.omissions,
            vec![Omission {
                domain: EvidenceDomain::Symbol,
                count: 0,
                reason,
            }]
        );
        assert_eq!(
            evidence.temporal.freshness,
            if reason == OmissionReason::Stale {
                FreshnessState::Stale
            } else {
                FreshnessState::Unknown
            }
        );
    }
}

/// A graph query that fails after its projection opened, as reads do while a
/// generation is still being seated, answers the same typed state as a failed
/// open; a failure that did not come from the graph stays a failed read.
#[test]
fn graph_query_failures_after_open_refuse_like_a_failed_open() {
    let unavailable: RetrievalPortOutcome<QualifiedNamePrimitiveResult> = graph_query_outcome(
        &tracedecay_graph_query::map_code_graph_read_runtime_error(
            CodeGraphReadError::Unavailable {
                detail: "the projection closed".to_owned(),
            },
        ),
        EvidenceDomain::Graph,
        UtcMicros(1),
    );
    let RetrievalPortOutcome::Refused(_, problem) = &unavailable else {
        panic!("a graph read failure must refuse: {unavailable:?}");
    };
    assert_eq!(
        (problem.kind(), problem.reason_code()),
        (
            ApplicationProblemKind::Unavailable,
            "application.code-graph.unavailable"
        )
    );

    let unrelated: RetrievalPortOutcome<QualifiedNamePrimitiveResult> = graph_query_outcome(
        &tracedecay_domain::errors::TraceDecayError::Config {
            message: "not a graph read".to_owned(),
        },
        EvidenceDomain::Graph,
        UtcMicros(1),
    );
    assert_eq!(
        unrelated
            .into_termination()
            .map(|(termination, _)| termination),
        Ok(tracedecay_contracts::OperationTermination::Failed)
    );
}
