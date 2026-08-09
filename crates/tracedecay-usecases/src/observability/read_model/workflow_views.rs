//! Denominator-safe Workflow settlement projections.

use std::collections::BTreeMap;

use tracedecay_application::{MetricValueV1, ObservabilityHorizonV1};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, WorkflowLifecycleObservedV1,
    WorkflowOutcomeObservedV1, WorkflowResourceObservedV1, WorkflowRunStatus,
};

use super::{MetricSpec, metric, unknown_metric};

const WORKFLOW_DESCRIPTOR: &str = "workflow-runtime.v1";

pub(super) fn workflow_metrics(
    events: &[&ObservabilityEnvelopeV1],
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
    source_complete: bool,
    source_unknown: u64,
) -> Vec<MetricValueV1> {
    let lifecycle = latest_lifecycle(events);
    if lifecycle.is_empty() {
        return required_names()
            .into_iter()
            .map(|(name, unit, denominator)| {
                unknown_metric(
                    WORKFLOW_DESCRIPTOR,
                    name,
                    unit,
                    denominator,
                    "workflow_settlements_not_recorded",
                    horizon,
                    watermark,
                )
            })
            .collect();
    }
    let outcome = latest_outcomes(events);
    let resource = latest_resources(events);
    let terminal_runs = lifecycle
        .values()
        .filter(|value| value.status.is_terminal())
        .count() as u64;
    let joined_outcomes = lifecycle
        .values()
        .filter(|value| value.status.is_terminal())
        .filter_map(|value| {
            outcome
                .get(&(value.run_id.as_str(), value.workflow_sequence))
                .copied()
                .filter(|outcome| {
                    outcome.status == value.status && outcome.total_steps == value.total_steps
                })
        })
        .collect::<Vec<_>>();
    let outcome_unknown = terminal_runs.saturating_sub(joined_outcomes.len() as u64);
    let outcome_known = source_complete
        && outcome_unknown == 0
        && joined_outcomes
            .iter()
            .all(|value| value.coverage == CoverageStateV1::Known);
    let outcome_coverage = if outcome_known {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let eligible_attempts = joined_outcomes
        .iter()
        .map(|value| u64::from(value.eligible_attempts))
        .sum::<u64>();
    let observed_attempts = joined_outcomes
        .iter()
        .map(|value| u64::from(value.observed_attempts))
        .sum::<u64>();
    let unknown_attempts = joined_outcomes
        .iter()
        .map(|value| u64::from(value.unknown_attempts))
        .sum::<u64>();
    let resource_rows = lifecycle
        .values()
        .filter_map(|value| {
            let key = (value.run_id.as_str(), value.workflow_sequence);
            let resource = resource.get(&key)?;
            if outcome
                .get(&key)
                .is_some_and(|outcome| outcome.eligible_attempts != resource.eligible_attempts)
            {
                return None;
            }
            Some(resource)
        })
        .collect::<Vec<_>>();
    let resource_known = source_complete
        && resource_rows.len() == lifecycle.len()
        && resource_rows
            .iter()
            .all(|value| value.coverage == CoverageStateV1::Known);
    let observed_duration = checked_optional_sum(
        resource_rows
            .iter()
            .map(|value| value.observed_duration_micros),
    );
    let critical_path = checked_optional_sum(
        resource_rows
            .iter()
            .map(|value| value.critical_path_duration_micros),
    );
    let artifacts = resource_rows.iter().try_fold(0_u64, |total, value| {
        total.checked_add(value.artifact_count)
    });
    let resource_known = resource_known
        && observed_duration.is_some()
        && critical_path.is_some()
        && artifacts.is_some();
    let resource_coverage = if resource_known {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let lifecycle_known = source_complete
        && source_unknown == 0
        && lifecycle
            .values()
            .all(|value| value.coverage == CoverageStateV1::Known);
    let lifecycle_coverage = if lifecycle_known {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    vec![
        value_metric(
            "runs",
            "count",
            "observed workflow runs",
            lifecycle_known.then_some(lifecycle.len() as f64),
            lifecycle_known.then_some(lifecycle.len() as u64),
            lifecycle.len() as u64,
            source_unknown,
            lifecycle_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "terminal_runs",
            "count",
            "observed workflow runs",
            lifecycle_known.then_some(terminal_runs as f64),
            lifecycle_known.then_some(lifecycle.len() as u64),
            lifecycle.len() as u64,
            source_unknown,
            lifecycle_coverage,
            horizon,
            watermark,
        ),
        status_metric(
            "succeeded_runs",
            WorkflowRunStatus::Completed,
            &joined_outcomes,
            lifecycle_known.then_some(terminal_runs),
            outcome_unknown,
            outcome_coverage,
            outcome_known,
            horizon,
            watermark,
        ),
        status_metric(
            "failed_runs",
            WorkflowRunStatus::Failed,
            &joined_outcomes,
            lifecycle_known.then_some(terminal_runs),
            outcome_unknown,
            outcome_coverage,
            outcome_known,
            horizon,
            watermark,
        ),
        status_metric(
            "cancelled_runs",
            WorkflowRunStatus::Cancelled,
            &joined_outcomes,
            lifecycle_known.then_some(terminal_runs),
            outcome_unknown,
            outcome_coverage,
            outcome_known,
            horizon,
            watermark,
        ),
        value_metric(
            "eligible_attempts",
            "count",
            "terminal workflow runs",
            outcome_known.then_some(eligible_attempts as f64),
            outcome_known.then_some(terminal_runs),
            joined_outcomes.len() as u64,
            unknown_attempts,
            outcome_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "observed_attempts",
            "count",
            "eligible workflow attempts",
            outcome_known.then_some(observed_attempts as f64),
            outcome_known.then_some(eligible_attempts),
            observed_attempts,
            unknown_attempts,
            outcome_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "unknown_attempts",
            "count",
            "eligible workflow attempts",
            outcome_known.then_some(0.0),
            outcome_known.then_some(eligible_attempts),
            observed_attempts,
            unknown_attempts,
            outcome_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "observed_duration",
            "microseconds",
            "workflow resource observations",
            resource_known
                .then_some(observed_duration)
                .flatten()
                .map(|value| value as f64),
            lifecycle_known.then_some(lifecycle.len() as u64),
            resource_rows.len() as u64,
            lifecycle.len().saturating_sub(resource_rows.len()) as u64,
            resource_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "critical_path_duration",
            "microseconds",
            "workflow resource observations",
            resource_known
                .then_some(critical_path)
                .flatten()
                .map(|value| value as f64),
            lifecycle_known.then_some(lifecycle.len() as u64),
            resource_rows.len() as u64,
            lifecycle.len().saturating_sub(resource_rows.len()) as u64,
            resource_coverage,
            horizon,
            watermark,
        ),
        value_metric(
            "artifacts",
            "count",
            "workflow resource observations",
            resource_known
                .then_some(artifacts)
                .flatten()
                .map(|value| value as f64),
            lifecycle_known.then_some(lifecycle.len() as u64),
            resource_rows.len() as u64,
            lifecycle.len().saturating_sub(resource_rows.len()) as u64,
            resource_coverage,
            horizon,
            watermark,
        ),
    ]
}

fn latest_lifecycle<'a>(
    events: &'a [&'a ObservabilityEnvelopeV1],
) -> BTreeMap<&'a str, &'a WorkflowLifecycleObservedV1> {
    let mut latest = BTreeMap::new();
    for event in events {
        if let ObservabilityPayloadV1::WorkflowLifecycle(value) = &event.payload {
            let replace = latest.get(value.run_id.as_str()).is_none_or(
                |current: &&WorkflowLifecycleObservedV1| {
                    value.workflow_sequence > current.workflow_sequence
                },
            );
            if replace {
                latest.insert(value.run_id.as_str(), value);
            }
        }
    }
    latest
}

fn latest_outcomes<'a>(
    events: &'a [&'a ObservabilityEnvelopeV1],
) -> BTreeMap<(&'a str, u64), &'a WorkflowOutcomeObservedV1> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::WorkflowOutcome(value) => {
                Some(((value.run_id.as_str(), value.workflow_sequence), value))
            }
            _ => None,
        })
        .collect()
}

fn latest_resources<'a>(
    events: &'a [&'a ObservabilityEnvelopeV1],
) -> BTreeMap<(&'a str, u64), &'a WorkflowResourceObservedV1> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::WorkflowResource(value) => {
                Some(((value.run_id.as_str(), value.workflow_sequence), value))
            }
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn status_metric(
    name: &'static str,
    status: WorkflowRunStatus,
    outcomes: &[&WorkflowOutcomeObservedV1],
    eligible: Option<u64>,
    unknown: u64,
    coverage: CoverageStateV1,
    known: bool,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
) -> MetricValueV1 {
    value_metric(
        name,
        "count",
        "terminal workflow runs",
        known.then_some(
            outcomes
                .iter()
                .filter(|value| value.status == status)
                .count() as f64,
        ),
        eligible,
        outcomes.len() as u64,
        unknown,
        coverage,
        horizon,
        watermark,
    )
}

#[allow(clippy::too_many_arguments)]
fn value_metric(
    name: &'static str,
    unit: &'static str,
    denominator: &'static str,
    value: Option<f64>,
    eligible: Option<u64>,
    observed: u64,
    unknown: u64,
    state: CoverageStateV1,
    horizon: &ObservabilityHorizonV1,
    watermark: &str,
) -> MetricValueV1 {
    metric(
        MetricSpec {
            descriptor: WORKFLOW_DESCRIPTOR,
            name,
            unit,
            denominator,
            value,
            eligible,
            observed,
            censored: 0,
            unknown,
            state,
            reason: (state != CoverageStateV1::Known).then_some("workflow_coverage_incomplete"),
        },
        horizon,
        watermark,
    )
}

fn checked_optional_sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0_u64, |total, value| total.checked_add(value?))
}

const fn required_names() -> [(&'static str, &'static str, &'static str); 11] {
    [
        ("runs", "count", "observed workflow runs"),
        ("terminal_runs", "count", "observed workflow runs"),
        ("succeeded_runs", "count", "terminal workflow runs"),
        ("failed_runs", "count", "terminal workflow runs"),
        ("cancelled_runs", "count", "terminal workflow runs"),
        ("eligible_attempts", "count", "terminal workflow runs"),
        ("observed_attempts", "count", "eligible workflow attempts"),
        ("unknown_attempts", "count", "eligible workflow attempts"),
        (
            "observed_duration",
            "microseconds",
            "workflow resource observations",
        ),
        (
            "critical_path_duration",
            "microseconds",
            "workflow resource observations",
        ),
        ("artifacts", "count", "workflow resource observations"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ManifestDigest, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, RunId,
        UtcMicros,
    };

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn envelope(id: &str, payload: ObservabilityPayloadV1) -> ObservabilityEnvelopeV1 {
        let event_kind = payload.event_kind().to_owned();
        ObservabilityEnvelopeV1 {
            event_id: id.to_owned(),
            event_kind,
            schema_revision: 1,
            idempotency_key: id.to_owned(),
            trace_id: "trace.workflow.metrics".to_owned(),
            scope_ref: "project.workflow.metrics".to_owned(),
            capability: "workflow".to_owned(),
            operation: "workflow_settlement".to_owned(),
            event_time_micros: 20,
            observation_time_micros: 20,
            valid_from_micros: Some(10),
            valid_until_micros: Some(20),
            quantity: Some(1.0),
            unit: Some("events".to_owned()),
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            producer_revision: "workflow-test.v1".to_owned(),
            configuration_revision: "workflow-config.v1".to_owned(),
            policy_revision: "workflow-policy.v1".to_owned(),
            watermark: "workflow:pending".to_owned(),
            coverage: CoverageStateV1::Known,
            sampling_probability: None,
            retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
            emitted_count: 1,
            delayed_count: 0,
            dropped_count: 0,
            process_boot_id: "boot.workflow.metrics".to_owned(),
            producer_sequence: 1,
            payload,
        }
    }

    fn lifecycle() -> WorkflowLifecycleObservedV1 {
        WorkflowLifecycleObservedV1 {
            run_id: RunId::new("run.workflow.metrics").unwrap(),
            workflow_sequence: 4,
            definition_ref: "workflow.definition.metrics".to_owned(),
            definition_version: 1,
            topology_digest: digest('a'),
            provider_registry_digest: digest('b'),
            status: WorkflowRunStatus::Completed,
            started_at: UtcMicros(10),
            observed_at: UtcMicros(20),
            total_steps: 1,
            coverage: CoverageStateV1::Known,
        }
    }

    #[test]
    fn terminal_outcome_and_resources_project_only_with_complete_denominators() {
        let events = vec![
            envelope(
                "workflow:lifecycle",
                ObservabilityPayloadV1::WorkflowLifecycle(lifecycle()),
            ),
            envelope(
                "workflow:outcome",
                ObservabilityPayloadV1::WorkflowOutcome(WorkflowOutcomeObservedV1 {
                    run_id: RunId::new("run.workflow.metrics").unwrap(),
                    workflow_sequence: 4,
                    status: WorkflowRunStatus::Completed,
                    total_steps: 1,
                    succeeded_steps: 1,
                    failed_steps: 0,
                    cancelled_steps: 0,
                    eligible_attempts: 2,
                    observed_attempts: 2,
                    succeeded_attempts: 2,
                    failed_attempts: 0,
                    timed_out_attempts: 0,
                    cancelled_attempts: 0,
                    unknown_attempts: 0,
                    coverage: CoverageStateV1::Known,
                }),
            ),
            envelope(
                "workflow:resource",
                ObservabilityPayloadV1::WorkflowResource(WorkflowResourceObservedV1 {
                    run_id: RunId::new("run.workflow.metrics").unwrap(),
                    workflow_sequence: 4,
                    eligible_attempts: 2,
                    observed_attempts: 2,
                    artifact_count: 3,
                    observed_duration_micros: Some(200),
                    critical_path_duration_micros: Some(150),
                    coverage: CoverageStateV1::Known,
                }),
            ),
        ];
        let refs = events.iter().collect::<Vec<_>>();
        let metrics = workflow_metrics(
            &refs,
            &ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 100,
            },
            "watermark:workflow",
            true,
            0,
        );
        let metric = |name: &str| metrics.iter().find(|value| value.metric == name).unwrap();
        assert_eq!(metric("succeeded_runs").value, Some(1.0));
        assert_eq!(metric("eligible_attempts").value, Some(2.0));
        assert_eq!(metric("observed_duration").value, Some(200.0));
        assert_eq!(metric("artifacts").value, Some(3.0));
    }

    #[test]
    fn missing_terminal_outcome_withholds_rate_inputs() {
        let event = envelope(
            "workflow:lifecycle-only",
            ObservabilityPayloadV1::WorkflowLifecycle(lifecycle()),
        );
        let metrics = workflow_metrics(
            &[&event],
            &ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 100,
            },
            "watermark:workflow",
            true,
            0,
        );
        let succeeded = metrics
            .iter()
            .find(|value| value.metric == "succeeded_runs")
            .unwrap();
        assert_eq!(succeeded.value, None);
        assert_eq!(succeeded.coverage.eligible, Some(1));
        assert_eq!(succeeded.coverage.unknown, 1);
        assert_eq!(succeeded.coverage.state, CoverageStateV1::Partial);
    }
}
