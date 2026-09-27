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
