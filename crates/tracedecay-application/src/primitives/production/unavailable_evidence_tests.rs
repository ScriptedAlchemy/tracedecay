use super::super::runtime::QualifiedNamePrimitiveResult;
use super::*;

#[test]
fn graph_admission_failures_preserve_termination_and_omission_reason() {
    for (error, reason) in [
        (
            CodeGraphReadError::Unavailable {
                detail: "the verified graph is not ready".to_owned(),
            },
            OmissionReason::Unavailable,
        ),
        (
            CodeGraphReadError::Stale {
                detail: "the graph no longer matches the request".to_owned(),
            },
            OmissionReason::Stale,
        ),
        (CodeGraphReadError::Cancelled, OmissionReason::Cancelled),
        (CodeGraphReadError::TimedOut, OmissionReason::TimedOut),
    ] {
        let outcome: RetrievalPortOutcome<QualifiedNamePrimitiveResult> =
            graph_read_outcome(&error, EvidenceDomain::Symbol, UtcMicros(1));
        match reason {
            OmissionReason::Cancelled => {
                assert!(matches!(&outcome, RetrievalPortOutcome::Cancelled(_)));
            }
            OmissionReason::TimedOut => {
                assert!(matches!(&outcome, RetrievalPortOutcome::TimedOut(_)));
            }
            _ => assert!(matches!(&outcome, RetrievalPortOutcome::Unavailable(_))),
        }
        let evidence = outcome.evidence();
        assert!(evidence.payload.is_none());
        assert!(evidence.coverage.validate().is_ok());
        assert_eq!(
            evidence.coverage.completeness,
            CoverageCompleteness::Unknown
        );
        assert_eq!(evidence.coverage.returned, 0);
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
