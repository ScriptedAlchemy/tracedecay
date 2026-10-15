#![allow(
    clippy::disallowed_methods,
    reason = "tests run on an ambient Tokio runtime"
)]
use std::time::Duration;

use tracedecay_contracts::{Deadline, RequestId};
use tracedecay_domain::UtcMicros;

use super::cycle::*;
use super::model::*;
use super::*;
use crate::operation_stream::OperationEventAuthority;

#[test]
fn interrupted_cycle_has_no_delivery_publication() {
    let outcome = AdvisoryCycleOutcome::Cancelled {
        contributions: AdvisoryContributionsV1::absent(),
    };
    assert!(outcome.publication().is_none());
}

#[test]
fn unrequested_remote_providers_are_typed_unavailable_not_omitted() {
    let mut contributions = AdvisoryContributionsV1::absent();
    mark_unrequested_remote_providers(&mut contributions, false, false);
    assert_eq!(
        contributions
            .providers
            .iter()
            .map(|provider| provider.state)
            .collect::<Vec<_>>(),
        vec![
            ProviderEvaluationStateV1::Unavailable,
            ProviderEvaluationStateV1::Unavailable,
            ProviderEvaluationStateV1::Absent,
        ]
    );
    assert_eq!(
        contributions
            .providers
            .iter()
            .take(2)
            .map(provider_state_event)
            .collect::<Vec<_>>(),
        vec![
            FeedbackSourceEventV1::ProviderState {
                provider: FeedbackAdvisoryProviderV1::GitHubReview,
                state: ProviderEvaluationStateV1::Unavailable,
            },
            FeedbackSourceEventV1::ProviderState {
                provider: FeedbackAdvisoryProviderV1::CiLocalization,
                state: ProviderEvaluationStateV1::Unavailable,
            },
        ]
    );
}

#[test]
fn ci_discovery_degradation_never_collapses_to_clean() {
    assert_eq!(
        ci_discovery_terminal_state(&ProductionCiFailureDiscoveryOutcomeV1::NotFound),
        Some((
            ProviderEvaluationStateV1::SupportedCompletedComplete,
            FeedbackOutcomeV1::Completed,
            FeedbackCoverageV1::Known,
        ))
    );
    for (discovery, expected) in [
        (
            ProductionCiFailureDiscoveryOutcomeV1::Ambiguous,
            ProviderEvaluationStateV1::Failed,
        ),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Denied,
            ProviderEvaluationStateV1::Unavailable,
        ),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Stale,
            ProviderEvaluationStateV1::Stale,
        ),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable,
            ProviderEvaluationStateV1::Unavailable,
        ),
        (
            ProductionCiFailureDiscoveryOutcomeV1::NotConfigured,
            ProviderEvaluationStateV1::Unavailable,
        ),
    ] {
        assert_eq!(
            ci_discovery_terminal_state(&discovery).map(|state| state.0),
            Some(expected)
        );
    }
}

/// A provider still running at the cycle deadline ends the wait as timed out,
/// so one stalled provider cannot hold the cycle past its budget.
#[tokio::test]
async fn a_provider_pending_at_the_cycle_deadline_times_out() {
    let operation = OperationEventAuthority::default()
        .begin_managed_test_run(
            "file:///workspace".to_owned(),
            RequestId::new("request.advisory.deadline").expect("request id"),
            Deadline::new(UtcMicros(i64::MAX - 1)).expect("deadline"),
        )
        .await
        .expect("operation");
    let deadline = Instant::now() + Duration::from_millis(50);
    let mut control = AdvisoryCycleControl {
        operation,
        deadline: MonotonicDeadline::at(deadline),
    };

    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        await_provider(&mut control, std::future::pending::<()>()),
    )
    .await
    .expect("the cycle deadline must end a pending provider wait");

    assert!(
        Instant::now() >= deadline,
        "the deadline must not fire early"
    );
    assert!(matches!(outcome, Err(AdvisoryCycleInterruption::TimedOut)));
}
