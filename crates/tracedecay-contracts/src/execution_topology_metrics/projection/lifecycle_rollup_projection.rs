use tracedecay_domain::CoverageStateV1;

use crate::observability::{MetricCoverageV1, MetricEvidenceClassV1};

use super::super::support::{
    MeasurementInput, as_f64, count_refusal, distribution_refusal, distribution_state, measurement,
    measurement_with_local_support, rate_refusal, ratio, seconds, union_micros,
};
use super::super::{
    ExecutionDeliveryOutcomeV1, ExecutionMetricUnavailableV1, ExecutionTopologyDimensionV1,
    ExecutionTopologyMeasurementV1,
};
use super::lifecycle_rollup::{
    ExecutionTopologyLifecycleCarryV1, ExecutionTopologyLifecycleRollupV1, apply_carry_to_rollup,
};
use super::{ExecutionTopologyRollupStateErrorV1, ProjectionContext};

impl ExecutionTopologyLifecycleRollupV1 {
    pub(in crate::execution_topology_metrics) fn project_with_carry(
        &self,
        carry: &ExecutionTopologyLifecycleCarryV1,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        let mut aggregate = self.clone();
        apply_carry_to_rollup(&mut aggregate, carry)?;
        project_merge_rollup(&aggregate, context, out);
        project_stale_stack_rollup(&aggregate, context, out);
        project_blocked_rollup(&aggregate, context, out);
        project_rerun_rollup(&aggregate, context, out);
        project_leak_rollup(&aggregate, context, out);
        project_delivery_rollup(&aggregate, context, out);
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn project_github_stack_capability(
        &self,
        context: &ProjectionContext,
    ) -> super::super::ExecutionGitHubStackCapabilityReadingV1 {
        let Some(row) = &self.github_stack_capability else {
            return super::super::ExecutionGitHubStackCapabilityReadingV1 {
                capability: None,
                standard_git_fallback_available: None,
                other_forge_fallback_available: None,
                coverage: MetricCoverageV1 {
                    eligible: context.complete.then_some(0),
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: u64::from(!context.complete),
                    excluded: 0,
                    state: context.source_state,
                },
                unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            };
        };
        let trusted = context.complete && row.coverage == CoverageStateV1::Known;
        super::super::ExecutionGitHubStackCapabilityReadingV1 {
            capability: trusted.then_some(row.capability.into()),
            standard_git_fallback_available: trusted.then_some(row.standard_git_fallback_available),
            other_forge_fallback_available: trusted.then_some(row.other_forge_fallback_available),
            coverage: MetricCoverageV1 {
                eligible: context.complete.then_some(1),
                observed: u64::from(row.coverage == CoverageStateV1::Known),
                completed: u64::from(row.coverage == CoverageStateV1::Known),
                censored: 0,
                unknown: u64::from(row.coverage != CoverageStateV1::Known),
                excluded: 0,
                state: if context.complete {
                    row.coverage
                } else {
                    context.source_state
                },
            },
            unavailable: (!trusted).then_some(ExecutionMetricUnavailableV1::CoverageFloorUnmet),
        }
    }
}

fn project_stale_stack_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let eligible = aggregate.stack_drift_eligible;
    let observed = eligible.saturating_sub(aggregate.stack_drift_unknown);
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(eligible),
        observed,
        completed: observed,
        censored: 0,
        unknown: aggregate.stack_drift_unknown,
        excluded: 0,
        state: distribution_state(context.complete, eligible, observed),
    };
    let refusal = distribution_refusal(context.complete, eligible, observed);
    if aggregate.stack_drift_cells.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_stale_stack_age_seconds",
            unit: "events",
            denominator: "observed_stack_drifts",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: Some(refusal.unwrap_or(ExecutionMetricUnavailableV1::NoEligibleEvidence)),
            context,
        }));
        return;
    }
    for ((kind, state, bucket), total) in &aggregate.stack_drift_cells {
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_stale_stack_age_seconds",
                unit: "events",
                denominator: "observed_stack_drifts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::StackDriftKind(*kind),
                    ExecutionTopologyDimensionV1::IntervalState(*state),
                    ExecutionTopologyDimensionV1::DurationBucket(*bucket),
                ],
                coverage: coverage.clone(),
                value: refusal.is_none().then_some(as_f64(*total)),
                unavailable: refusal,
                context,
            },
            *total,
        ));
    }
}

fn project_merge_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let eligible = aggregate.merge_eligible;
    let observed = eligible.saturating_sub(aggregate.merge_unknown);
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(eligible),
        observed,
        completed: observed,
        censored: 0,
        unknown: aggregate.merge_unknown,
        excluded: 0,
        state: distribution_state(context.complete, eligible, observed),
    };
    let count_reason = count_refusal(context.complete, eligible);
    for ((kind, outcome), total) in &aggregate.merge_cells {
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_merge_attempts_total",
                unit: "events",
                denominator: "observed_native_integrations",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::IntegrationKind(*kind),
                    ExecutionTopologyDimensionV1::IntegrationOutcome(*outcome),
                ],
                coverage: coverage.clone(),
                value: count_reason.is_none().then_some(as_f64(*total)),
                unavailable: count_reason,
                context,
            },
            *total,
        ));
    }
    if aggregate.merge_cells.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_merge_attempts_total",
            unit: "events",
            denominator: "observed_native_integrations",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage: coverage.clone(),
            value: None,
            unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            context,
        }));
    }
    for (kind, (total, succeeded)) in &aggregate.merge_totals {
        let reason = rate_refusal(context.complete, *total, *total);
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_merge_success_ratio",
                unit: "ratio",
                denominator: "observed_native_integrations",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::IntegrationKind(*kind)],
                coverage: coverage.clone(),
                value: reason
                    .is_none()
                    .then(|| ratio(*succeeded, *total))
                    .flatten(),
                unavailable: reason,
                context,
            },
            *total,
        ));
    }
    if aggregate.merge_totals.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_merge_success_ratio",
            unit: "ratio",
            denominator: "observed_native_integrations",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: rate_refusal(context.complete, eligible, observed),
            context,
        }));
    }
}

fn project_blocked_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let eligible = aggregate.blocked_eligible;
    let observed = aggregate.blocked_observed;
    let unknown = aggregate.blocked_unknown;
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(eligible),
        observed,
        completed: observed,
        censored: aggregate.blocked_censored,
        unknown,
        excluded: 0,
        state: if unknown > 0 {
            CoverageStateV1::Partial
        } else {
            distribution_state(context.complete, eligible, observed)
        },
    };
    let refusal = if unknown > 0 {
        Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet)
    } else if aggregate.blocked_censored > 0 && observed == 0 {
        Some(ExecutionMetricUnavailableV1::UnboundedInterval)
    } else {
        distribution_refusal(context.complete, eligible, observed)
    };
    let mut wall = aggregate.blocked_union.clone();
    out.push(measurement(MeasurementInput {
        metric: "work_blocked_wall_seconds",
        unit: "seconds",
        denominator: "closed_blocked_intervals",
        evidence_class: MetricEvidenceClassV1::Measurement,
        dimensions: Vec::new(),
        coverage: coverage.clone(),
        value: refusal.is_none().then(|| seconds(union_micros(&mut wall))),
        unavailable: refusal,
        context,
    }));
    for (cause, intervals) in &aggregate.blocked_cause_unions {
        let mut intervals = intervals.clone();
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_blocked_cause_seconds",
                unit: "seconds",
                denominator: "closed_blocked_intervals",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::BlockedCause(*cause)],
                coverage: coverage.clone(),
                value: refusal
                    .is_none()
                    .then(|| seconds(union_micros(&mut intervals))),
                unavailable: refusal,
                context,
            },
            aggregate
                .blocked_observed_by_cause
                .get(cause)
                .copied()
                .unwrap_or(0),
        ));
    }
    if aggregate.blocked_cause_unions.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_blocked_cause_seconds",
            unit: "seconds",
            denominator: "closed_blocked_intervals",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: refusal,
            context,
        }));
    }
}

fn project_rerun_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let eligible = aggregate.rerun_eligible;
    let observed = eligible.saturating_sub(aggregate.rerun_unknown);
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(eligible),
        observed,
        completed: observed,
        censored: 0,
        unknown: aggregate.rerun_unknown,
        excluded: 0,
        state: distribution_state(context.complete, eligible, observed),
    };
    let count_reason = count_refusal(context.complete, eligible);
    for ((source, cause), total) in &aggregate.rerun_cells {
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_reruns_total",
                unit: "events",
                denominator: "eligible_original_attempts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::RerunSource(*source),
                    ExecutionTopologyDimensionV1::RerunCause(*cause),
                ],
                coverage: coverage.clone(),
                value: count_reason.is_none().then_some(as_f64(*total)),
                unavailable: count_reason,
                context,
            },
            aggregate
                .rerun_eligible_cells
                .get(&(*source, *cause))
                .copied()
                .unwrap_or(0),
        ));
    }
    if aggregate.rerun_cells.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_reruns_total",
            unit: "events",
            denominator: "eligible_original_attempts",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage: coverage.clone(),
            value: None,
            unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            context,
        }));
    }
    for (source, (source_eligible, linked)) in &aggregate.rerun_totals {
        let reason = rate_refusal(context.complete, *source_eligible, *source_eligible);
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_rerun_rate",
                unit: "ratio",
                denominator: "eligible_original_attempts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::RerunSource(*source)],
                coverage: coverage.clone(),
                value: reason
                    .is_none()
                    .then(|| ratio(*linked, *source_eligible))
                    .flatten(),
                unavailable: reason,
                context,
            },
            *source_eligible,
        ));
    }
    if aggregate.rerun_totals.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_rerun_rate",
            unit: "ratio",
            denominator: "eligible_original_attempts",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: rate_refusal(context.complete, eligible, observed),
            context,
        }));
    }
}

fn project_leak_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let eligible = aggregate.leak_eligible;
    let observed = eligible.saturating_sub(aggregate.leak_unknown);
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(eligible),
        observed,
        completed: observed,
        censored: 0,
        unknown: aggregate.leak_unknown,
        excluded: 0,
        state: distribution_state(context.complete, eligible, observed),
    };
    let refusal = count_refusal(context.complete, eligible);
    for ((kind, outcome), total) in &aggregate.leak_cells {
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_execution_leaks_total",
                unit: "events",
                denominator: "observed_leak_detections",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::LeakKind(*kind),
                    ExecutionTopologyDimensionV1::LeakOutcome(*outcome),
                ],
                coverage: coverage.clone(),
                value: refusal.is_none().then_some(as_f64(*total)),
                unavailable: refusal,
                context,
            },
            *total,
        ));
    }
    if aggregate.leak_cells.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_execution_leaks_total",
            unit: "events",
            denominator: "observed_leak_detections",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            context,
        }));
    }
}

fn project_delivery_rollup(
    aggregate: &ExecutionTopologyLifecycleRollupV1,
    context: &ProjectionContext,
    out: &mut Vec<ExecutionTopologyMeasurementV1>,
) {
    let attempted = aggregate.delivery_attempted;
    let observed = attempted.saturating_sub(aggregate.delivery_unknown);
    let coverage = MetricCoverageV1 {
        eligible: context.complete.then_some(attempted),
        observed,
        completed: aggregate.delivery_completed,
        censored: aggregate.delivery_dropped,
        unknown: aggregate.delivery_unknown,
        excluded: 0,
        state: distribution_state(context.complete, attempted, observed),
    };
    let refusal = distribution_refusal(context.complete, attempted, observed);
    if aggregate.delivery_totals.is_empty() {
        out.push(measurement(MeasurementInput {
            metric: "work_delivery_fanout_total",
            unit: "events",
            denominator: "attempted_deliveries",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage: coverage.clone(),
            value: None,
            unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            context,
        }));
        out.push(measurement(MeasurementInput {
            metric: "work_delivery_duplicate_ratio",
            unit: "ratio",
            denominator: "attempted_deliveries",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: None,
            unavailable: refusal,
            context,
        }));
        return;
    }
    const OUTCOMES: [ExecutionDeliveryOutcomeV1; 4] = [
        ExecutionDeliveryOutcomeV1::Delivered,
        ExecutionDeliveryOutcomeV1::Deduplicated,
        ExecutionDeliveryOutcomeV1::Dropped,
        ExecutionDeliveryOutcomeV1::Unknown,
    ];
    for (surface, totals) in &aggregate.delivery_totals {
        for (index, outcome) in OUTCOMES.iter().enumerate() {
            let total = totals[index.saturating_add(1)];
            out.push(measurement_with_local_support(
                MeasurementInput {
                    metric: "work_delivery_fanout_total",
                    unit: "events",
                    denominator: "attempted_deliveries",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::Surface(*surface),
                        ExecutionTopologyDimensionV1::DeliveryOutcome(*outcome),
                    ],
                    coverage: coverage.clone(),
                    value: refusal.is_none().then_some(as_f64(total)),
                    unavailable: refusal,
                    context,
                },
                total,
            ));
        }
        let surface_attempted = totals[0];
        let reason = refusal
            .or((surface_attempted == 0)
                .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
        out.push(measurement_with_local_support(
            MeasurementInput {
                metric: "work_delivery_duplicate_ratio",
                unit: "ratio",
                denominator: "attempted_deliveries",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::Surface(*surface)],
                coverage: coverage.clone(),
                value: reason
                    .is_none()
                    .then(|| ratio(totals[2], surface_attempted))
                    .flatten(),
                unavailable: reason,
                context,
            },
            surface_attempted,
        ));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::execution_topology_metrics::RATE_MIN_ELIGIBLE_CASES_V1;
    use crate::observability::ObservabilityHorizonV1;

    #[test]
    fn empty_conditional_descriptors_preserve_their_family_coverage() {
        let rate_eligible = RATE_MIN_ELIGIBLE_CASES_V1;
        let aggregate = ExecutionTopologyLifecycleRollupV1 {
            merge_eligible: rate_eligible,
            merge_unknown: rate_eligible,
            blocked_eligible: 1,
            blocked_censored: 1,
            rerun_eligible: rate_eligible,
            rerun_unknown: rate_eligible,
            delivery_attempted: 1,
            delivery_unknown: 1,
            ..ExecutionTopologyLifecycleRollupV1::default()
        };
        let context = ProjectionContext {
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 1,
            },
            watermark: "analytics:family-coverage".to_owned(),
            complete: true,
            source_state: CoverageStateV1::Known,
        };
        let mut measurements = Vec::new();
        aggregate
            .project_with_carry(
                &ExecutionTopologyLifecycleCarryV1::default(),
                &context,
                &mut measurements,
            )
            .unwrap();

        for (metric, unavailable, coverage) in [
            (
                "work_merge_success_ratio",
                ExecutionMetricUnavailableV1::CoverageFloorUnmet,
                MetricCoverageV1 {
                    eligible: Some(rate_eligible),
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: rate_eligible,
                    excluded: 0,
                    state: CoverageStateV1::Partial,
                },
            ),
            (
                "work_blocked_cause_seconds",
                ExecutionMetricUnavailableV1::UnboundedInterval,
                MetricCoverageV1 {
                    eligible: Some(1),
                    observed: 0,
                    completed: 0,
                    censored: 1,
                    unknown: 0,
                    excluded: 0,
                    state: CoverageStateV1::Partial,
                },
            ),
            (
                "work_rerun_rate",
                ExecutionMetricUnavailableV1::CoverageFloorUnmet,
                MetricCoverageV1 {
                    eligible: Some(rate_eligible),
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: rate_eligible,
                    excluded: 0,
                    state: CoverageStateV1::Partial,
                },
            ),
            (
                "work_delivery_duplicate_ratio",
                ExecutionMetricUnavailableV1::CoverageFloorUnmet,
                MetricCoverageV1 {
                    eligible: Some(1),
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: 1,
                    excluded: 0,
                    state: CoverageStateV1::Partial,
                },
            ),
        ] {
            let measurement = measurements
                .iter()
                .find(|measurement| {
                    measurement.value.metric == metric && measurement.dimensions.is_empty()
                })
                .unwrap();
            assert_eq!(measurement.value.value, None, "metric={metric}");
            assert_eq!(
                measurement.unavailable,
                Some(unavailable),
                "metric={metric}"
            );
            assert_eq!(measurement.value.coverage, coverage, "metric={metric}");
            assert_eq!(
                measurement.value.denominator_value, coverage.eligible,
                "metric={metric}"
            );
            assert_ne!(
                measurement.unavailable,
                Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
                "metric={metric}"
            );
        }
    }
}
