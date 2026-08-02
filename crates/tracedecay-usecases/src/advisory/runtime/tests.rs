use super::cycle::*;
use super::model::*;
use super::*;

#[test]
fn absent_provider_states_remain_explicit() {
    let advisory = Pr13AdvisoryContributionsV1::absent()
        .as_plan09()
        .expect("canonical advisory");
    assert_eq!(
        advisory.provider_states,
        vec![
            ProviderEvaluationStateV1::Absent,
            ProviderEvaluationStateV1::Absent,
            ProviderEvaluationStateV1::Absent,
        ]
    );
    assert!(advisory.findings.is_empty());
}

#[test]
fn interrupted_cycle_has_no_delivery_publication() {
    let outcome = Pr13AdvisoryCycleOutcomeV1::Cancelled {
        contributions: Pr13AdvisoryContributionsV1::absent(),
    };
    assert!(outcome.publication().is_none());
}

#[test]
fn provider_state_events_preserve_each_closed_provider_identity() {
    let events = Pr13AdvisoryContributionsV1::absent()
        .providers
        .iter()
        .map(provider_state_event)
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::GitHubReview,
                state: ProviderEvaluationStateV1::Absent,
            },
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::CiLocalization,
                state: ProviderEvaluationStateV1::Absent,
            },
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::Proximity,
                state: ProviderEvaluationStateV1::Absent,
            },
        ]
    );
}

#[test]
fn unrequested_remote_providers_are_typed_unavailable_not_omitted() {
    let mut contributions = Pr13AdvisoryContributionsV1::absent();
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
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::GitHubReview,
                state: ProviderEvaluationStateV1::Unavailable,
            },
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::CiLocalization,
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
            Plan26FeedbackOutcomeV1::Completed,
            Plan26CoverageV1::Known,
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
