//! Observatory projections for reliance, automation, task intelligence, provider, and remote health.

use std::collections::BTreeSet;

use tracedecay_application::{MetricValueV1, ObservabilityHorizonV1};
use tracedecay_domain::{
    AutomationFunnelObservedV1, AutomationTerminalV1, CoverageStateV1, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservedTernaryV1, ProviderAttemptTerminalV1, RelianceDecisionV1,
    RelianceVerificationV1, TaskOutcomeV1,
};

use super::{MetricSpec, metric, unknown_metric};

const RELIANCE_DESCRIPTOR: &str = "appropriate-reliance.v1";
const AUTOMATION_DESCRIPTOR: &str = "automation-funnel.v1";
const TASK_INTELLIGENCE_DESCRIPTOR: &str = "task-intelligence.v1";
const PROVIDER_RELIABILITY_DESCRIPTOR: &str = "provider-reliability.v1";
const REMOTE_COVERAGE_DESCRIPTOR: &str = "remote-coverage.v1";

pub(super) fn product_view_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let mut metrics = reliance_metrics(events, horizon, watermark, source_complete, source_unknown);
    metrics.extend(automation_funnel_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(task_intelligence_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(provider_reliability_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics.extend(remote_coverage_metrics(
        events,
        horizon,
        watermark,
        source_complete,
        source_unknown,
    ));
    metrics
}

fn reliance_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let observations: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::AppropriateReliance(value) => Some(value),
            _ => None,
        })
        .collect();
    let specifications = [
        (
            "accepted_correct",
            observations
                .iter()
                .filter(|value| {
                    value.decision == RelianceDecisionV1::Accepted
                        && value.verification == RelianceVerificationV1::Correct
                        && value.independently_verified
                })
                .count(),
        ),
        (
            "accepted_incorrect",
            observations
                .iter()
                .filter(|value| {
                    value.decision == RelianceDecisionV1::Accepted
                        && value.verification == RelianceVerificationV1::Incorrect
                        && value.independently_verified
                })
                .count(),
        ),
        (
            "rejected_correct",
            observations
                .iter()
                .filter(|value| {
                    value.decision == RelianceDecisionV1::Rejected
                        && value.verification == RelianceVerificationV1::Correct
                        && value.independently_verified
                })
                .count(),
        ),
        (
            "rejected_incorrect",
            observations
                .iter()
                .filter(|value| {
                    value.decision == RelianceDecisionV1::Rejected
                        && value.verification == RelianceVerificationV1::Incorrect
                        && value.independently_verified
                })
                .count(),
        ),
        (
            "override_with_rationale",
            observations
                .iter()
                .filter(|value| {
                    value.decision == RelianceDecisionV1::Overridden
                        && value.override_rationale_present
                })
                .count(),
        ),
        (
            "no_eligible_verification",
            observations
                .iter()
                .filter(|value| {
                    value.verification == RelianceVerificationV1::NoEligibleVerification
                })
                .count(),
        ),
        (
            "unknown_or_censored",
            observations
                .iter()
                .filter(|value| {
                    matches!(
                        value.verification,
                        RelianceVerificationV1::Unknown | RelianceVerificationV1::Censored
                    )
                })
                .count(),
        ),
    ];
    count_metrics(
        RELIANCE_DESCRIPTOR,
        &specifications,
        observations.len(),
        "reliance_observations_not_recorded",
        horizon,
        watermark,
        source_complete,
        source_unknown,
    )
}

fn automation_funnel_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let mut seen_runs = BTreeSet::new();
    let observations: Vec<_> = events
        .iter()
        .rev()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::AutomationFunnel(value)
                if seen_runs.insert(value.run_ref.as_str()) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect();
    if observations.is_empty() {
        return [
            "eligible",
            "admitted",
            "executed",
            "useful_work",
            "effect",
            "recovery",
            "terminal_succeeded",
        ]
        .into_iter()
        .map(|name| {
            unknown_metric(
                AUTOMATION_DESCRIPTOR,
                name,
                "count",
                "observations",
                "automation_run_ledger_observations_not_recorded",
                horizon,
                watermark,
            )
        })
        .collect();
    }
    let ternary = |name, select: fn(&AutomationFunnelObservedV1) -> ObservedTernaryV1| {
        automation_metric(
            name,
            &observations,
            |value| select(value) == ObservedTernaryV1::Yes,
            |value| select(value) == ObservedTernaryV1::Unknown,
            horizon,
            watermark,
            source_complete,
            source_unknown,
        )
    };
    vec![
        ternary("eligible", |value| value.eligible),
        ternary("admitted", |value| value.admitted),
        ternary("executed", |value| value.executed),
        ternary("useful_work", |value| value.useful_work),
        ternary("effect", |value| value.effect),
        ternary("recovery", |value| value.recovery),
        automation_metric(
            "terminal_succeeded",
            &observations,
            |value| value.terminal == AutomationTerminalV1::Succeeded,
            |value| !value.terminal.is_terminal(),
            horizon,
            watermark,
            source_complete,
            source_unknown,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn automation_metric(
    name: &'static str,
    observations: &[&AutomationFunnelObservedV1],
    is_yes: impl Fn(&AutomationFunnelObservedV1) -> bool,
    is_unknown: impl Fn(&AutomationFunnelObservedV1) -> bool,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> MetricValueV1 {
    let value = observations.iter().filter(|value| is_yes(value)).count();
    let payload_unknown = observations
        .iter()
        .filter(|value| value.ledger_coverage != CoverageStateV1::Known || is_unknown(value))
        .count() as u64;
    let unknown = source_unknown.saturating_add(payload_unknown);
    let mut state = observations
        .iter()
        .fold(CoverageStateV1::Known, |state, value| {
            weaker_coverage(state, value.ledger_coverage)
        });
    if payload_unknown > 0 && state == CoverageStateV1::Known {
        state = CoverageStateV1::Partial;
    }
    if !source_complete {
        state = weaker_coverage(state, CoverageStateV1::Partial);
    }
    if source_unknown > 0 {
        state = weaker_coverage(state, CoverageStateV1::Partial);
    }
    let reason = match state {
        CoverageStateV1::Known => None,
        CoverageStateV1::Capped => Some("automation_run_ledger_scan_capped"),
        CoverageStateV1::Unknown => Some("automation_run_ledger_coverage_unknown"),
        CoverageStateV1::Stale => Some("automation_run_ledger_coverage_stale"),
        CoverageStateV1::Partial | CoverageStateV1::Sampled => {
            Some("automation_metric_evidence_incomplete")
        }
    };
    metric(
        MetricSpec {
            descriptor: AUTOMATION_DESCRIPTOR,
            name,
            unit: "count",
            denominator: "observations",
            value: (state == CoverageStateV1::Known).then_some(value as f64),
            eligible: Some(observations.len() as u64),
            observed: observations.len() as u64 - payload_unknown,
            censored: 0,
            unknown,
            state,
            reason,
        },
        horizon,
        watermark,
    )
}

const fn coverage_rank(state: CoverageStateV1) -> u8 {
    match state {
        CoverageStateV1::Known => 0,
        CoverageStateV1::Sampled => 1,
        CoverageStateV1::Capped => 2,
        CoverageStateV1::Partial => 3,
        CoverageStateV1::Stale => 4,
        CoverageStateV1::Unknown => 5,
    }
}

const fn weaker_coverage(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    if coverage_rank(left) >= coverage_rank(right) {
        left
    } else {
        right
    }
}

fn task_intelligence_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let decisions: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::TaskIntelligenceDecision(value) => Some(value),
            _ => None,
        })
        .collect();
    let outcomes: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::TaskIntelligenceOutcome(value) => Some(value),
            _ => None,
        })
        .collect();
    let joined_outcomes = outcomes
        .iter()
        .filter(|outcome| {
            decisions
                .iter()
                .any(|decision| decision.proposal_ref == outcome.proposal_ref)
        })
        .count();
    let specifications = [
        ("decisions", decisions.len()),
        (
            "calibrated_estimates",
            decisions
                .iter()
                .filter(|value| value.calibration.is_some())
                .count(),
        ),
        (
            "drift_invalid",
            decisions
                .iter()
                .filter(|value| {
                    value
                        .calibration
                        .as_ref()
                        .is_some_and(|calibration| !calibration.drift_valid)
                })
                .count(),
        ),
        (
            "decomposition_proposed",
            decisions
                .iter()
                .filter(|value| {
                    value
                        .decomposition_candidate_count
                        .is_some_and(|count| count > 0)
                })
                .count(),
        ),
        (
            "deterministic_fallback",
            decisions
                .iter()
                .filter(|value| value.deterministic_fallback)
                .count(),
        ),
        ("joined_terminal_outcomes", joined_outcomes),
        (
            "terminal_succeeded",
            outcomes
                .iter()
                .filter(|value| value.outcome == TaskOutcomeV1::Succeeded)
                .count(),
        ),
        (
            "outcome_join_unknown",
            outcomes.len().saturating_sub(joined_outcomes),
        ),
    ];
    count_metrics(
        TASK_INTELLIGENCE_DESCRIPTOR,
        &specifications,
        decisions.len().saturating_add(outcomes.len()),
        "task_intelligence_observations_not_recorded",
        horizon,
        watermark,
        source_complete,
        source_unknown,
    )
}

fn provider_reliability_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let observations: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::ProviderReliability(value) => Some(value),
            _ => None,
        })
        .collect();
    let specifications = [
        ("attempts", observations.len()),
        (
            "succeeded",
            observations
                .iter()
                .filter(|value| value.terminal == ProviderAttemptTerminalV1::Succeeded)
                .count(),
        ),
        (
            "failed",
            observations
                .iter()
                .filter(|value| value.terminal == ProviderAttemptTerminalV1::Failed)
                .count(),
        ),
        (
            "timed_out",
            observations
                .iter()
                .filter(|value| value.terminal == ProviderAttemptTerminalV1::TimedOut)
                .count(),
        ),
        (
            "cancelled",
            observations
                .iter()
                .filter(|value| value.terminal == ProviderAttemptTerminalV1::Cancelled)
                .count(),
        ),
        (
            "fallback",
            ternary_yes(&observations, |value| value.fallback),
        ),
        (
            "progress",
            ternary_yes(&observations, |value| value.progress),
        ),
        (
            "recovery",
            ternary_yes(&observations, |value| value.recovery),
        ),
        (
            "with_artifacts",
            observations
                .iter()
                .filter(|value| value.artifact_count > 0)
                .count(),
        ),
        (
            "claude_code_cli_attempts",
            observations
                .iter()
                .filter(|value| value.backend == "claude_code_cli")
                .count(),
        ),
        (
            "codex_app_server_attempts",
            observations
                .iter()
                .filter(|value| value.backend == "codex_app_server")
                .count(),
        ),
        (
            "codex_cli_attempts",
            observations
                .iter()
                .filter(|value| value.backend == "codex_cli")
                .count(),
        ),
        (
            "cancellation_observed",
            ternary_yes(&observations, |value| value.cancellation),
        ),
        (
            "effect_unknown",
            observations
                .iter()
                .filter(|value| value.effect == ObservedTernaryV1::Unknown)
                .count(),
        ),
        (
            "usage_join_unknown",
            observations
                .iter()
                .filter(|value| value.usage_coverage != CoverageStateV1::Known)
                .count(),
        ),
    ];
    let mut metrics = count_metrics(
        PROVIDER_RELIABILITY_DESCRIPTOR,
        &specifications,
        observations.len(),
        "provider_attempt_observations_not_recorded",
        horizon,
        watermark,
        source_complete,
        source_unknown,
    );
    let correlated_usage: Vec<_> = observations
        .iter()
        .filter(|value| value.usage_coverage == CoverageStateV1::Known)
        .collect();
    let currencies = correlated_usage
        .iter()
        .filter_map(|value| value.cost_currency.as_deref())
        .collect::<BTreeSet<_>>();
    for (name, unit, value) in [
        (
            "input_tokens",
            "tokens",
            correlated_usage
                .iter()
                .filter_map(|value| value.input_tokens.map(|tokens| tokens as f64))
                .sum::<f64>(),
        ),
        (
            "output_tokens",
            "tokens",
            correlated_usage
                .iter()
                .filter_map(|value| value.output_tokens.map(|tokens| tokens as f64))
                .sum::<f64>(),
        ),
        (
            "cost_amount",
            "currency_amount",
            correlated_usage
                .iter()
                .filter_map(|value| value.cost_amount)
                .sum::<f64>(),
        ),
    ] {
        if correlated_usage.is_empty() {
            metrics.push(unknown_metric(
                PROVIDER_RELIABILITY_DESCRIPTOR,
                name,
                unit,
                "correlated provider attempts",
                "provider_usage_not_correlated_to_work_attempt",
                horizon,
                watermark,
            ));
        } else if name == "cost_amount" && currencies.len() != 1 {
            metrics.push(unknown_metric(
                PROVIDER_RELIABILITY_DESCRIPTOR,
                name,
                unit,
                "correlated provider attempts",
                "mixed_provider_cost_currencies",
                horizon,
                watermark,
            ));
        } else {
            metrics.push(metric(
                MetricSpec {
                    descriptor: PROVIDER_RELIABILITY_DESCRIPTOR,
                    name,
                    unit,
                    denominator: "correlated provider attempts",
                    value: Some(value),
                    eligible: Some(observations.len() as u64),
                    observed: correlated_usage.len() as u64,
                    censored: 0,
                    unknown: observations.len().saturating_sub(correlated_usage.len()) as u64,
                    state: CoverageStateV1::Partial,
                    reason: Some("provider_usage_join_partial"),
                },
                horizon,
                watermark,
            ));
        }
    }
    metrics
}

fn remote_coverage_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let observations: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::RemoteCoverage(value) => Some(value),
            _ => None,
        })
        .collect();
    let specifications = [
        ("operations", observations.len()),
        (
            "complete",
            observations
                .iter()
                .filter(|value| value.coverage == CoverageStateV1::Known)
                .count(),
        ),
        (
            "partial",
            observations
                .iter()
                .filter(|value| value.coverage == CoverageStateV1::Partial)
                .count(),
        ),
        (
            "stale",
            observations
                .iter()
                .filter(|value| value.coverage == CoverageStateV1::Stale)
                .count(),
        ),
        (
            "unknown_or_unavailable",
            observations
                .iter()
                .filter(|value| {
                    matches!(
                        value.coverage,
                        CoverageStateV1::Unknown | CoverageStateV1::Capped
                    )
                })
                .count(),
        ),
        (
            "pending_local_evidence",
            observations
                .iter()
                .filter_map(|value| value.pending_local_evidence.map(|count| count as usize))
                .sum(),
        ),
    ];
    count_metrics(
        REMOTE_COVERAGE_DESCRIPTOR,
        &specifications,
        observations.len(),
        "remote_protocol_outcomes_not_retained",
        horizon,
        watermark,
        source_complete,
        source_unknown,
    )
}

fn ternary_yes<T>(observations: &[&T], select: impl Fn(&T) -> ObservedTernaryV1) -> usize {
    observations
        .iter()
        .filter(|value| select(value) == ObservedTernaryV1::Yes)
        .count()
}

#[allow(clippy::too_many_arguments)]
fn count_metrics(
    descriptor: &'static str,
    specifications: &[(&'static str, usize)],
    observation_count: usize,
    unavailable_reason: &'static str,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    if observation_count == 0 {
        return specifications
            .iter()
            .map(|(name, _)| {
                unknown_metric(
                    descriptor,
                    name,
                    "count",
                    "observations",
                    unavailable_reason,
                    horizon,
                    watermark,
                )
            })
            .collect();
    }
    specifications
        .iter()
        .map(|(name, value)| {
            metric(
                MetricSpec {
                    descriptor,
                    name,
                    unit: "count",
                    denominator: "observations",
                    value: Some(*value as f64),
                    eligible: Some(observation_count as u64),
                    observed: observation_count as u64,
                    censored: 0,
                    unknown: source_unknown,
                    state: if source_complete {
                        CoverageStateV1::Known
                    } else {
                        CoverageStateV1::Partial
                    },
                    reason: (!source_complete).then_some("incomplete_observability_coverage"),
                },
                horizon,
                watermark,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        AutomationFunnelObservedV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    };

    fn automation_envelope(observation: AutomationFunnelObservedV1) -> ObservabilityEnvelopeV1 {
        ObservabilityEnvelopeV1 {
            event_id: "automation:test".to_owned(),
            event_kind: "automation.funnel.observed.v1".to_owned(),
            schema_revision: 1,
            idempotency_key: "automation:test".to_owned(),
            trace_id: "automation:test".to_owned(),
            scope_ref: "project.automation".to_owned(),
            capability: "work".to_owned(),
            operation: "automation_run_lifecycle".to_owned(),
            event_time_micros: 10,
            observation_time_micros: 10,
            valid_from_micros: Some(10),
            valid_until_micros: None,
            quantity: Some(1.0),
            unit: Some("events".to_owned()),
            terminal_result: Some(ObservabilityTerminalResultV1::Unknown),
            producer_revision: "automation-test.v1".to_owned(),
            configuration_revision: "automation-config.v1".to_owned(),
            policy_revision: "automation-policy.v1".to_owned(),
            watermark: "automation:1".to_owned(),
            coverage: observation.ledger_coverage,
            sampling_probability: None,
            retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
            emitted_count: 1,
            delayed_count: 0,
            dropped_count: 0,
            process_boot_id: "boot.automation".to_owned(),
            producer_sequence: 1,
            payload: ObservabilityPayloadV1::AutomationFunnel(observation),
        }
    }

    #[test]
    fn automation_unknown_ledger_withholds_every_point_value() {
        let event = automation_envelope(AutomationFunnelObservedV1 {
            run_ref: "automation.run.unknown".to_owned(),
            ledger_coverage: CoverageStateV1::Unknown,
            eligible: ObservedTernaryV1::Unknown,
            admitted: ObservedTernaryV1::Unknown,
            executed: ObservedTernaryV1::Unknown,
            useful_work: ObservedTernaryV1::Unknown,
            effect: ObservedTernaryV1::Unknown,
            recovery: ObservedTernaryV1::Unknown,
            terminal: AutomationTerminalV1::Running,
        });
        let metrics = automation_funnel_metrics(
            &[&event],
            &ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 20,
            },
            "automation:1",
            true,
            0,
        );

        assert!(!metrics.is_empty());
        assert!(metrics.iter().all(|metric| metric.value.is_none()));
        assert!(
            metrics
                .iter()
                .all(|metric| metric.coverage.state == CoverageStateV1::Unknown)
        );
    }

    #[test]
    fn automation_ternary_unknown_only_withholds_affected_metric() {
        let event = automation_envelope(AutomationFunnelObservedV1 {
            run_ref: "automation.run.metric-specific".to_owned(),
            ledger_coverage: CoverageStateV1::Known,
            eligible: ObservedTernaryV1::Unknown,
            admitted: ObservedTernaryV1::Yes,
            executed: ObservedTernaryV1::No,
            useful_work: ObservedTernaryV1::No,
            effect: ObservedTernaryV1::No,
            recovery: ObservedTernaryV1::No,
            terminal: AutomationTerminalV1::Succeeded,
        });
        let metrics = automation_funnel_metrics(
            &[&event],
            &ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 20,
            },
            "automation:1",
            true,
            0,
        );
        let metric = |name: &str| metrics.iter().find(|metric| metric.metric == name).unwrap();

        assert_eq!(metric("eligible").value, None);
        assert_eq!(metric("eligible").coverage.state, CoverageStateV1::Partial);
        assert_eq!(metric("admitted").value, Some(1.0));
        assert_eq!(metric("executed").value, Some(0.0));
        assert_eq!(metric("terminal_succeeded").value, Some(1.0));
    }
}
