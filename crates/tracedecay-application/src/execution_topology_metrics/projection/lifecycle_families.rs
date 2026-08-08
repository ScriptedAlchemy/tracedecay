use std::collections::BTreeMap;

use tracedecay_domain::{BlockedCauseV1, CoverageStateV1, IntegrationPhaseV1, IntegrationResultV1};

use crate::observability::{MetricCoverageV1, MetricEvidenceClassV1};

use super::super::support::{
    MeasurementInput, as_f64, blocked_key, count, count_refusal, count_state, distribution_refusal,
    distribution_state, measurement, rate_refusal, ratio, seconds, union_micros,
};
use super::super::{
    ALL_DURATION_BUCKETS_V1, ExecutionDeliveryOutcomeV1, ExecutionDriftKindV1,
    ExecutionDurationBucketV1, ExecutionIntegrationKindV1, ExecutionIntegrationOutcomeV1,
    ExecutionIntervalStateV1, ExecutionLeakKindV1, ExecutionLeakOutcomeV1,
    ExecutionMetricUnavailableV1, ExecutionRerunCauseV1, ExecutionRerunSourceV1,
    ExecutionSurfaceFamilyV1, ExecutionTopologyDimensionV1, ExecutionTopologyMeasurementV1,
    duration_bucket,
};
use super::{BlockedRowV1, ExecutionTopologyEvidenceV1, IntegrationRowV1, ProjectionContext};

impl ExecutionTopologyEvidenceV1 {
    /// `work_ready_to_integrated_seconds{integration_kind}` — from the first
    /// `Ready` valid time to the first exact `NativeIntegratedObserved`, per
    /// authorized local join reference. A trace that reaches `Cancelled` or
    /// `Censored`, or never reaches a native integration inside the horizon,
    /// is censored and never counted as a zero or a success.
    pub(super) fn project_ready_to_integrated(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let mut eligible = 0u64;
        let mut observed = 0u64;
        let mut censored = 0u64;
        let mut cells: BTreeMap<(ExecutionIntegrationKindV1, ExecutionDurationBucketV1), u64> =
            BTreeMap::new();
        let mut kinds: Vec<ExecutionIntegrationKindV1> = Vec::new();
        for rows in self.integration_traces.values() {
            let mut ready_at: Option<i64> = None;
            let mut integrated: Option<IntegrationRowV1> = None;
            let mut terminated = false;
            for row in rows {
                match row.phase {
                    IntegrationPhaseV1::Ready => {
                        let start = row.valid_from_micros.unwrap_or(row.event_time_micros);
                        ready_at = Some(ready_at.map_or(start, |current: i64| current.min(start)));
                    }
                    IntegrationPhaseV1::NativeIntegratedObserved => {
                        let earlier = match integrated {
                            None => true,
                            Some(current) => row.event_time_micros < current.event_time_micros,
                        };
                        if earlier {
                            integrated = Some(*row);
                        }
                    }
                    IntegrationPhaseV1::Cancelled | IntegrationPhaseV1::Censored => {
                        terminated = true;
                    }
                    _ => {}
                }
            }
            let Some(start) = ready_at else {
                // No `Ready` valid time for this join reference, so there is
                // no interval to start; the trace is not eligible at all.
                continue;
            };
            eligible = eligible.saturating_add(1);
            match integrated {
                Some(row) if !terminated && row.event_time_micros >= start => {
                    let elapsed = row.event_time_micros.saturating_sub(start);
                    let bucket = duration_bucket(u64::try_from(elapsed).unwrap_or(0));
                    let kind = ExecutionIntegrationKindV1::from(row.operation);
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                    let entry = cells.entry((kind, bucket)).or_insert(0);
                    *entry = entry.saturating_add(1);
                    observed = observed.saturating_add(1);
                }
                _ => censored = censored.saturating_add(1),
            }
        }
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored,
            unknown: 0,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = rate_refusal(context.complete, eligible, observed);
        if let Some(reason) = refusal {
            out.push(measurement(MeasurementInput {
                metric: "work_ready_to_integrated_seconds",
                unit: "events",
                denominator: "ready_work_item_versions",
                evidence_class: MetricEvidenceClassV1::Association,
                dimensions: Vec::new(),
                coverage,
                value: None,
                unavailable: Some(reason),
                context,
            }));
            return;
        }
        kinds.sort_unstable();
        for kind in kinds {
            for bucket in ALL_DURATION_BUCKETS_V1 {
                let total = cells.get(&(kind, bucket)).copied().unwrap_or(0);
                out.push(measurement(MeasurementInput {
                    metric: "work_ready_to_integrated_seconds",
                    unit: "events",
                    denominator: "ready_work_item_versions",
                    evidence_class: MetricEvidenceClassV1::Association,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::IntegrationKind(kind),
                        ExecutionTopologyDimensionV1::DurationBucket(bucket),
                    ],
                    coverage: coverage.clone(),
                    value: Some(as_f64(total)),
                    unavailable: None,
                    context,
                }));
            }
        }
    }

    /// `work_merge_attempts_total{integration_kind,outcome}` and
    /// `work_merge_success_ratio{integration_kind}` — observed native
    /// integrations only. Required checks and accepted task outcomes stay in
    /// their own dimensions and never inflate this numerator.
    pub(super) fn project_merge(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let mut cells: BTreeMap<(ExecutionIntegrationKindV1, ExecutionIntegrationOutcomeV1), u64> =
            BTreeMap::new();
        let mut per_kind: BTreeMap<ExecutionIntegrationKindV1, (u64, u64)> = BTreeMap::new();
        let mut eligible = 0u64;
        let mut unknown = 0u64;
        for row in &self.integrations {
            if row.phase != IntegrationPhaseV1::NativeIntegratedObserved {
                continue;
            }
            eligible = eligible.saturating_add(1);
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(1);
            }
            let kind = ExecutionIntegrationKindV1::from(row.operation);
            let outcome = ExecutionIntegrationOutcomeV1::from(row.result);
            let cell = cells.entry((kind, outcome)).or_insert(0);
            *cell = cell.saturating_add(1);
            let totals = per_kind.entry(kind).or_insert((0, 0));
            totals.0 = totals.0.saturating_add(1);
            if row.result == IntegrationResultV1::Succeeded {
                totals.1 = totals.1.saturating_add(1);
            }
        }
        let observed = eligible.saturating_sub(unknown);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored: 0,
            unknown,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let count_reason = count_refusal(context.complete, eligible);
        for ((kind, outcome), total) in &cells {
            out.push(measurement(MeasurementInput {
                metric: "work_merge_attempts_total",
                unit: "events",
                denominator: "observed_native_integrations",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::IntegrationKind(*kind),
                    ExecutionTopologyDimensionV1::IntegrationOutcome(*outcome),
                ],
                coverage: coverage.clone(),
                value: if count_reason.is_some() {
                    None
                } else {
                    Some(as_f64(*total))
                },
                unavailable: count_reason,
                context,
            }));
        }
        if cells.is_empty() {
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
        for (kind, (total, succeeded)) in &per_kind {
            let reason = rate_refusal(context.complete, *total, *total);
            out.push(measurement(MeasurementInput {
                metric: "work_merge_success_ratio",
                unit: "ratio",
                denominator: "observed_native_integrations",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::IntegrationKind(*kind)],
                coverage: coverage.clone(),
                value: if reason.is_some() {
                    None
                } else {
                    ratio(*succeeded, *total)
                },
                unavailable: reason,
                context,
            }));
        }
    }

    /// `work_stale_stack_age_seconds{drift_kind,state}` — bucketed age of
    /// proved invalidating observations. An open interval keeps its state
    /// rather than being closed at read time.
    pub(super) fn project_stale_stack(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let eligible = count(self.drifts.len());
        let mut unknown = 0u64;
        let mut cells: BTreeMap<
            (
                ExecutionDriftKindV1,
                ExecutionIntervalStateV1,
                ExecutionDurationBucketV1,
            ),
            u64,
        > = BTreeMap::new();
        for row in &self.drifts {
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(1);
            }
            let key = (row.kind.into(), row.state.into(), row.age_bucket);
            let entry = cells.entry(key).or_insert(0);
            *entry = entry.saturating_add(1);
        }
        let observed = eligible.saturating_sub(unknown);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored: 0,
            unknown,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = distribution_refusal(context.complete, eligible, observed);
        if let Some(reason) = refusal {
            out.push(measurement(MeasurementInput {
                metric: "work_stale_stack_age_seconds",
                unit: "events",
                denominator: "observed_stack_drifts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: Vec::new(),
                coverage,
                value: None,
                unavailable: Some(reason),
                context,
            }));
            return;
        }
        for ((kind, state, bucket), total) in &cells {
            out.push(measurement(MeasurementInput {
                metric: "work_stale_stack_age_seconds",
                unit: "events",
                denominator: "observed_stack_drifts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::DriftKind(*kind),
                    ExecutionTopologyDimensionV1::IntervalState(*state),
                    ExecutionTopologyDimensionV1::DurationBucket(*bucket),
                ],
                coverage: coverage.clone(),
                value: Some(as_f64(*total)),
                unavailable: None,
                context,
            }));
        }
    }

    /// `work_blocked_wall_seconds` and `work_blocked_cause_seconds{cause}`.
    /// Wall time is the union of every blocked interval; per-cause time is
    /// the union within that cause only, so overlapping causes may sum above
    /// wall time by construction. An interval with no proved terminal cannot
    /// be measured and is censored.
    pub(super) fn project_blocked(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        // Intervals are versioned: the same interval may be restated with a
        // higher revision, and only the latest revision is evidence.
        let mut latest: BTreeMap<(&'static str, i64), BlockedRowV1> = BTreeMap::new();
        for row in &self.blocked {
            let key = (blocked_key(row.cause), row.valid_from_micros);
            let supersedes = match latest.get(&key) {
                Some(existing) => row.revision > existing.revision,
                None => true,
            };
            if supersedes {
                latest.insert(key, *row);
            }
        }
        let eligible = count(latest.len());
        let mut closed: Vec<(i64, i64)> = Vec::new();
        let mut per_cause: BTreeMap<&'static str, (BlockedCauseV1, Vec<(i64, i64)>)> =
            BTreeMap::new();
        let mut censored = 0u64;
        for row in latest.values() {
            let Some(until) = row.valid_until_micros else {
                censored = censored.saturating_add(1);
                continue;
            };
            closed.push((row.valid_from_micros, until));
            let entry = per_cause
                .entry(blocked_key(row.cause))
                .or_insert_with(|| (row.cause, Vec::new()));
            entry.1.push((row.valid_from_micros, until));
        }
        let observed = count(closed.len());
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored,
            unknown: 0,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = if censored > 0 && observed == 0 {
            Some(ExecutionMetricUnavailableV1::UnboundedInterval)
        } else {
            distribution_refusal(context.complete, eligible, observed)
        };
        out.push(measurement(MeasurementInput {
            metric: "work_blocked_wall_seconds",
            unit: "seconds",
            denominator: "closed_blocked_intervals",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage: coverage.clone(),
            value: if refusal.is_some() {
                None
            } else {
                Some(seconds(union_micros(&mut closed)))
            },
            unavailable: refusal,
            context,
        }));
        for (cause, intervals) in per_cause.values_mut() {
            out.push(measurement(MeasurementInput {
                metric: "work_blocked_cause_seconds",
                unit: "seconds",
                denominator: "closed_blocked_intervals",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::BlockedCause((*cause).into())],
                coverage: coverage.clone(),
                value: if refusal.is_some() {
                    None
                } else {
                    Some(seconds(union_micros(intervals)))
                },
                unavailable: refusal,
                context,
            }));
        }
    }

    /// `work_reruns_total{source,cause}` and `work_rerun_rate{source}` —
    /// denominated by eligible original attempts and runs. Transport replay
    /// carries no rerun identity and never reaches these events.
    pub(super) fn project_reruns(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let mut cells: BTreeMap<(ExecutionRerunSourceV1, ExecutionRerunCauseV1), u64> =
            BTreeMap::new();
        let mut per_source: BTreeMap<ExecutionRerunSourceV1, (u64, u64)> = BTreeMap::new();
        let mut eligible = 0u64;
        let mut unknown = 0u64;
        for row in &self.reruns {
            eligible = eligible.saturating_add(row.eligible);
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(row.eligible);
            }
            let cell = cells
                .entry((row.source.into(), row.cause.into()))
                .or_insert(0);
            *cell = cell.saturating_add(row.linked);
            let totals = per_source.entry(row.source.into()).or_insert((0, 0));
            totals.0 = totals.0.saturating_add(row.eligible);
            totals.1 = totals.1.saturating_add(row.linked);
        }
        let observed = eligible.saturating_sub(unknown);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored: 0,
            unknown,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let count_reason = count_refusal(context.complete, eligible);
        for ((source, cause), total) in &cells {
            out.push(measurement(MeasurementInput {
                metric: "work_reruns_total",
                unit: "events",
                denominator: "eligible_original_attempts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::RerunSource(*source),
                    ExecutionTopologyDimensionV1::RerunCause(*cause),
                ],
                coverage: coverage.clone(),
                value: if count_reason.is_some() {
                    None
                } else {
                    Some(as_f64(*total))
                },
                unavailable: count_reason,
                context,
            }));
        }
        if cells.is_empty() {
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
        for (source, (source_eligible, linked)) in &per_source {
            let reason = rate_refusal(context.complete, *source_eligible, *source_eligible);
            out.push(measurement(MeasurementInput {
                metric: "work_rerun_rate",
                unit: "ratio",
                denominator: "eligible_original_attempts",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::RerunSource(*source)],
                coverage: coverage.clone(),
                value: if reason.is_some() {
                    None
                } else {
                    ratio(*linked, *source_eligible)
                },
                unavailable: reason,
                context,
            }));
        }
    }

    /// `work_execution_leaks_total{kind,outcome}` — proved leaks only, with
    /// unknown-coverage observations retained rather than dropped.
    pub(super) fn project_leaks(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let eligible = count(self.leaks.len());
        let mut unknown = 0u64;
        let mut cells: BTreeMap<(ExecutionLeakKindV1, ExecutionLeakOutcomeV1), u64> =
            BTreeMap::new();
        for row in &self.leaks {
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(1);
            }
            let cell = cells
                .entry((row.kind.into(), row.recovery.into()))
                .or_insert(0);
            *cell = cell.saturating_add(1);
        }
        let observed = eligible.saturating_sub(unknown);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored: 0,
            unknown,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = count_refusal(context.complete, eligible);
        for ((kind, outcome), total) in &cells {
            out.push(measurement(MeasurementInput {
                metric: "work_execution_leaks_total",
                unit: "events",
                denominator: "observed_leak_detections",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![
                    ExecutionTopologyDimensionV1::LeakKind(*kind),
                    ExecutionTopologyDimensionV1::LeakOutcome(*outcome),
                ],
                coverage: coverage.clone(),
                value: if refusal.is_some() {
                    None
                } else {
                    Some(as_f64(*total))
                },
                unavailable: refusal,
                context,
            }));
        }
        if cells.is_empty() {
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

    /// `work_delivery_fanout_total{surface,outcome}` and
    /// `work_delivery_duplicate_ratio{surface}`. Delivering one event to
    /// several surfaces is fanout, not duplicated product work; only the
    /// deduplicated count feeds the duplicate ratio.
    pub(super) fn project_delivery(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let mut per_surface: BTreeMap<ExecutionSurfaceFamilyV1, [u64; 5]> = BTreeMap::new();
        let mut attempted_total = 0u64;
        for row in &self.fanout {
            let entry = per_surface.entry(row.surface.into()).or_insert([0; 5]);
            entry[0] = entry[0].saturating_add(row.attempted);
            entry[1] = entry[1].saturating_add(row.delivered);
            entry[2] = entry[2].saturating_add(row.deduplicated);
            entry[3] = entry[3].saturating_add(row.dropped);
            entry[4] = entry[4].saturating_add(row.unknown);
            attempted_total = attempted_total.saturating_add(row.attempted);
        }
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(attempted_total),
            observed: attempted_total,
            completed: attempted_total,
            censored: 0,
            unknown: 0,
            excluded: 0,
            state: count_state(context.complete),
        };
        let refusal = count_refusal(context.complete, attempted_total);
        if per_surface.is_empty() {
            out.push(measurement(MeasurementInput {
                metric: "work_delivery_fanout_total",
                unit: "events",
                denominator: "attempted_deliveries",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: Vec::new(),
                coverage,
                value: None,
                unavailable: Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
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
        for (surface, totals) in &per_surface {
            for (index, outcome) in OUTCOMES.iter().enumerate() {
                let total = totals[index.saturating_add(1)];
                out.push(measurement(MeasurementInput {
                    metric: "work_delivery_fanout_total",
                    unit: "events",
                    denominator: "attempted_deliveries",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::Surface(*surface),
                        ExecutionTopologyDimensionV1::DeliveryOutcome(*outcome),
                    ],
                    coverage: coverage.clone(),
                    value: if refusal.is_some() {
                        None
                    } else {
                        Some(as_f64(total))
                    },
                    unavailable: refusal,
                    context,
                }));
            }
            let attempted = totals[0];
            let reason = refusal
                .or((attempted == 0).then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement(MeasurementInput {
                metric: "work_delivery_duplicate_ratio",
                unit: "ratio",
                denominator: "attempted_deliveries",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::Surface(*surface)],
                coverage: coverage.clone(),
                value: if reason.is_some() {
                    None
                } else {
                    ratio(totals[2], attempted)
                },
                unavailable: reason,
                context,
            }));
        }
    }
}
