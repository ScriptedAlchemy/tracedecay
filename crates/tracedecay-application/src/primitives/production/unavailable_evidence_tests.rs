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
        &GraphQueryError::Read(CodeGraphReadError::Unavailable {
            detail: "the projection closed".to_owned(),
        }),
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

    for error in [
        tracedecay_domain::errors::TraceDecayError::Config {
            message: "not a graph read".to_owned(),
        },
        tracedecay_domain::errors::TraceDecayError::project_route(
            "unrelated-query-failure",
            false,
            "not a graph read",
        ),
    ] {
        let unrelated: RetrievalPortOutcome<QualifiedNamePrimitiveResult> = graph_query_outcome(
            &GraphQueryError::Runtime(error),
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
}

#[test]
fn graph_query_refusals_keep_operator_and_retry_details() {
    for (error, expected) in [
        (
            CodeGraphReadError::Parked {
                cause: "projection store cannot be opened".to_owned(),
                remedy: "repair the configured store path".to_owned(),
                retries_on_wake: false,
            },
            ApplicationProblemDetailV1::Parked {
                cause: "projection store cannot be opened".to_owned(),
                remedy: "repair the configured store path".to_owned(),
                retries_on_wake: false,
            },
        ),
        (
            CodeGraphReadError::Rewarming {
                retry_after_millis: 47,
            },
            ApplicationProblemDetailV1::CodeGraphRewarming {
                retry_after_millis: 47,
            },
        ),
    ] {
        let outcome: RetrievalPortOutcome<QualifiedNamePrimitiveResult> = graph_query_outcome(
            &GraphQueryError::Read(error),
            EvidenceDomain::Graph,
            UtcMicros(1),
        );
        let RetrievalPortOutcome::Refused(_, problem) = outcome else {
            panic!("operator and retry details require a refusal: {outcome:?}");
        };
        assert_eq!(problem.detail(), Some(&expected));
    }
}

#[test]
fn health_delta_definition_refusals_keep_their_code_and_restart_without_cursor() {
    use crate::graph_health_delta::HealthDeltaError;

    for (error, code) in [
        (
            HealthDeltaError::DefinitionMismatch {
                recorded: "health.2026-01-01".to_owned(),
            },
            "application.health-delta.definition-mismatch",
        ),
        (
            HealthDeltaError::DefinitionUnrecorded,
            "application.health-delta.definition-unrecorded",
        ),
    ] {
        let outcome: RetrievalPortOutcome<QualifiedNamePrimitiveResult> =
            health_delta_outcome(&error, UtcMicros(1));
        let RetrievalPortOutcome::Refused(_, problem) = &outcome else {
            panic!("{error} must refuse: {outcome:?}");
        };
        assert_eq!(problem.reason_code(), code, "{error}");
        assert!(
            problem
                .legal_actions()
                .contains(&tracedecay_contracts::LegalAction::RestartWithoutCursor),
            "{error}: {:?}",
            problem.legal_actions()
        );
    }
}
