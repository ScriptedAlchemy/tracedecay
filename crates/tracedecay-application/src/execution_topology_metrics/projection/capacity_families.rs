use std::collections::BTreeMap;

use tracedecay_domain::{
    ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1, CoverageStateV1,
    DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
};

use crate::observability::{MetricCoverageV1, MetricEvidenceClassV1};

use super::super::support::{
    MeasurementInput, as_f64, conflict_refusal, count, count_refusal, count_state,
    distribution_refusal, distribution_state, measurement, outcome_key, ratio,
};
use super::super::{
    ALL_QUANTITY_UNITS_V1, ALL_WIDTH_BUCKETS_V1, ExecutionConcurrencyPhaseV1,
    ExecutionFanoutPhaseV1, ExecutionMetricUnavailableV1, ExecutionTopologyDimensionV1,
    ExecutionTopologyMeasurementV1, ExecutionWidthBucketV1, width_bucket,
};
use super::{ExecutionTopologyEvidenceV1, ProjectionContext};

impl ExecutionTopologyEvidenceV1 {
    /// `work_execution_concurrency_width{phase}` — duration weighted. A
    /// sample without a bounded valid-time interval carries no duration and
    /// is censored, never counted as one unit of time.
    pub(super) fn project_concurrency_width(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        const PHASES: [ExecutionConcurrencyPhaseV1; 5] = [
            ExecutionConcurrencyPhaseV1::Requested,
            ExecutionConcurrencyPhaseV1::Accepted,
            ExecutionConcurrencyPhaseV1::Admitted,
            ExecutionConcurrencyPhaseV1::Active,
            ExecutionConcurrencyPhaseV1::Useful,
        ];
        let eligible = count(self.topology.len());
        let mut observed = 0u64;
        for sample in &self.topology {
            if sample.interval_micros.is_some() && sample.coverage == CoverageStateV1::Known {
                observed = observed.saturating_add(1);
            }
        }
        let censored = eligible.saturating_sub(observed);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored,
            unknown: 0,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = distribution_refusal(context.complete, eligible, observed);
        for (index, phase) in PHASES.iter().enumerate() {
            if let Some(reason) = refusal {
                out.push(measurement(MeasurementInput {
                    metric: "work_execution_concurrency_width",
                    unit: "microseconds",
                    denominator: "duration_weighted_topology_samples",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![ExecutionTopologyDimensionV1::ConcurrencyPhase(*phase)],
                    coverage: coverage.clone(),
                    value: None,
                    unavailable: Some(reason),
                    context,
                }));
                continue;
            }
            let mut buckets: BTreeMap<ExecutionWidthBucketV1, u64> = BTreeMap::new();
            for sample in &self.topology {
                let Some(duration) = sample.interval_micros else {
                    continue;
                };
                if sample.coverage != CoverageStateV1::Known {
                    continue;
                }
                let bucket = width_bucket(sample.widths[index]);
                let entry = buckets.entry(bucket).or_insert(0);
                *entry = entry.saturating_add(duration);
            }
            for bucket in ALL_WIDTH_BUCKETS_V1 {
                let micros = buckets.get(&bucket).copied().unwrap_or(0);
                out.push(measurement(MeasurementInput {
                    metric: "work_execution_concurrency_width",
                    unit: "microseconds",
                    denominator: "duration_weighted_topology_samples",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::ConcurrencyPhase(*phase),
                        ExecutionTopologyDimensionV1::WidthBucket(bucket),
                    ],
                    coverage: coverage.clone(),
                    value: Some(as_f64(micros)),
                    unavailable: None,
                    context,
                }));
            }
        }
    }

    /// `work_execution_useful_concurrency_ratio` — useful attempt-time over
    /// admitted attempt-time across intervals with known coverage.
    pub(super) fn project_useful_ratio(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let eligible = count(self.topology.len());
        let mut observed = 0u64;
        let mut useful_time = 0u64;
        let mut admitted_time = 0u64;
        for sample in &self.topology {
            let Some(duration) = sample.interval_micros else {
                continue;
            };
            if sample.coverage != CoverageStateV1::Known {
                continue;
            }
            observed = observed.saturating_add(1);
            useful_time =
                useful_time.saturating_add(u64::from(sample.widths[4]).saturating_mul(duration));
            admitted_time =
                admitted_time.saturating_add(u64::from(sample.widths[2]).saturating_mul(duration));
        }
        let censored = eligible.saturating_sub(observed);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored,
            unknown: 0,
            excluded: 0,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = distribution_refusal(context.complete, eligible, observed)
            .or((admitted_time == 0).then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
        out.push(measurement(MeasurementInput {
            metric: "work_execution_useful_concurrency_ratio",
            unit: "ratio",
            denominator: "admitted_attempt_micros",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: if refusal.is_some() {
                None
            } else {
                ratio(useful_time, admitted_time)
            },
            unavailable: refusal,
            context,
        }));
    }

    /// `work_execution_fanout_width{phase}` — the unweighted width
    /// distribution. It deliberately does not require an interval, so
    /// serialized and blocked samples are preserved rather than dropped.
    pub(super) fn project_fanout_width(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        const PHASES: [ExecutionFanoutPhaseV1; 5] = [
            ExecutionFanoutPhaseV1::Requested,
            ExecutionFanoutPhaseV1::Accepted,
            ExecutionFanoutPhaseV1::Admitted,
            ExecutionFanoutPhaseV1::PeakActive,
            ExecutionFanoutPhaseV1::Useful,
        ];
        let eligible = count(self.topology.len());
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed: eligible,
            completed: eligible,
            censored: 0,
            unknown: 0,
            excluded: 0,
            state: count_state(context.complete),
        };
        let refusal = count_refusal(context.complete, eligible);
        for (index, phase) in PHASES.iter().enumerate() {
            if let Some(reason) = refusal {
                out.push(measurement(MeasurementInput {
                    metric: "work_execution_fanout_width",
                    unit: "events",
                    denominator: "topology_samples",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![ExecutionTopologyDimensionV1::FanoutPhase(*phase)],
                    coverage: coverage.clone(),
                    value: None,
                    unavailable: Some(reason),
                    context,
                }));
                continue;
            }
            let mut buckets: BTreeMap<ExecutionWidthBucketV1, u64> = BTreeMap::new();
            for sample in &self.topology {
                let bucket = width_bucket(sample.widths[index]);
                let entry = buckets.entry(bucket).or_insert(0);
                *entry = entry.saturating_add(1);
            }
            for bucket in ALL_WIDTH_BUCKETS_V1 {
                let count = buckets.get(&bucket).copied().unwrap_or(0);
                out.push(measurement(MeasurementInput {
                    metric: "work_execution_fanout_width",
                    unit: "events",
                    denominator: "topology_samples",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::FanoutPhase(*phase),
                        ExecutionTopologyDimensionV1::WidthBucket(bucket),
                    ],
                    coverage: coverage.clone(),
                    value: Some(as_f64(count)),
                    unavailable: None,
                    context,
                }));
            }
        }
    }

    /// `work_duplicate_effort_total{kind,unit}` and
    /// `work_duplicate_effort_ratio{unit}`. Only adjudicated duplicate
    /// relations enter the numerator; `NotDuplicate` supplies the
    /// non-duplicate side of the denominator; censored and unknown relations
    /// enter neither.
    pub(super) fn project_duplicate_effort(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        const DUPLICATE_KINDS: [DuplicateEffortKindV1; 4] = [
            DuplicateEffortKindV1::ExactDuplicate,
            DuplicateEffortKindV1::SupersededOverlap,
            DuplicateEffortKindV1::RepeatedInvestigation,
            DuplicateEffortKindV1::DuplicateEffect,
        ];
        let eligible = count(self.duplicates.len());
        let mut adjudicated = 0u64;
        let mut censored = 0u64;
        let mut unknown = 0u64;
        for row in &self.duplicates {
            // A relation whose own recorded coverage is unknown is not an
            // adjudication, whatever kind it claims: counting it as observed
            // would let an unmeasured event raise the denominator.
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(1);
                continue;
            }
            match row.kind {
                DuplicateEffortKindV1::Censored => censored = censored.saturating_add(1),
                DuplicateEffortKindV1::Unknown => unknown = unknown.saturating_add(1),
                _ => adjudicated = adjudicated.saturating_add(1),
            }
        }
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed: adjudicated,
            completed: adjudicated,
            censored,
            unknown,
            excluded: 0,
            state: distribution_state(context.complete, eligible, adjudicated),
        };
        let refusal = count_refusal(context.complete, eligible);
        for (unit_index, unit) in ALL_QUANTITY_UNITS_V1.iter().enumerate() {
            let mut duplicate_total = 0u64;
            let mut population_total = 0u64;
            for row in &self.duplicates {
                let Some(quantity) = row.quantities[unit_index] else {
                    continue;
                };
                if row.coverage == CoverageStateV1::Unknown {
                    continue;
                }
                match row.kind {
                    DuplicateEffortKindV1::Censored | DuplicateEffortKindV1::Unknown => continue,
                    DuplicateEffortKindV1::NotDuplicate => {
                        population_total = population_total.saturating_add(quantity);
                    }
                    _ => {
                        duplicate_total = duplicate_total.saturating_add(quantity);
                        population_total = population_total.saturating_add(quantity);
                    }
                }
            }
            for kind in DUPLICATE_KINDS {
                let mut total = 0u64;
                for row in &self.duplicates {
                    if row.kind != kind || row.coverage == CoverageStateV1::Unknown {
                        continue;
                    }
                    if let Some(quantity) = row.quantities[unit_index] {
                        total = total.saturating_add(quantity);
                    }
                }
                out.push(measurement(MeasurementInput {
                    metric: "work_duplicate_effort_total",
                    unit: unit.wire_unit(),
                    denominator: "adjudicated_duplicate_relations",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::DuplicateKind(kind.into()),
                        ExecutionTopologyDimensionV1::Unit(*unit),
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
            let ratio_refusal = refusal
                .or((population_total == 0)
                    .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement(MeasurementInput {
                metric: "work_duplicate_effort_ratio",
                unit: "ratio",
                denominator: "adjudicated_effort_quantity",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::Unit(*unit)],
                coverage: coverage.clone(),
                value: if ratio_refusal.is_some() {
                    None
                } else {
                    ratio(duplicate_total, population_total)
                },
                unavailable: ratio_refusal,
                context,
            }));
        }
    }

    /// `work_duplicate_effects_total{outcome}` — a prevented duplicate never
    /// merges with a committed one.
    pub(super) fn project_duplicate_effects(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        const OUTCOMES: [DuplicateEffectOutcomeV1; 3] = [
            DuplicateEffectOutcomeV1::Prevented,
            DuplicateEffectOutcomeV1::Committed,
            DuplicateEffectOutcomeV1::Unknown,
        ];
        let mut eligible = 0u64;
        let mut excluded = 0u64;
        let mut unknown = 0u64;
        for row in &self.duplicates {
            if row.effect_outcome == DuplicateEffectOutcomeV1::NotApplicable {
                excluded = excluded.saturating_add(1);
                continue;
            }
            eligible = eligible.saturating_add(1);
            if row.coverage == CoverageStateV1::Unknown {
                unknown = unknown.saturating_add(1);
            }
        }
        let observed = eligible.saturating_sub(unknown);
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(eligible),
            observed,
            completed: observed,
            censored: 0,
            unknown,
            excluded,
            state: distribution_state(context.complete, eligible, observed),
        };
        let refusal = count_refusal(context.complete, eligible);
        for outcome in OUTCOMES {
            let mut total = 0u64;
            for row in &self.duplicates {
                if row.effect_outcome == outcome && row.coverage != CoverageStateV1::Unknown {
                    total = total.saturating_add(1);
                }
            }
            out.push(measurement(MeasurementInput {
                metric: "work_duplicate_effects_total",
                unit: "events",
                denominator: "observed_duplicate_effects",
                evidence_class: MetricEvidenceClassV1::Measurement,
                dimensions: vec![ExecutionTopologyDimensionV1::DuplicateOutcome(
                    outcome.into(),
                )],
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
    }

    /// `work_conflict_prediction_total{kind,outcome}`,
    /// `work_conflict_prediction_precision{kind}`, and
    /// `work_conflict_prediction_recall{kind}`. Only pre-integration
    /// predictions linked to an independently observed outcome of the same
    /// kind are eligible; unlinked, censored, and unknown cases stay out of
    /// every confusion-matrix denominator.
    pub(super) fn project_conflict(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        const KINDS: [ConflictKindV1; 3] = [
            ConflictKindV1::Mechanical,
            ConflictKindV1::Semantic,
            ConflictKindV1::Combined,
        ];
        const OUTCOMES: [ConflictOutcomeV1; 4] = [
            ConflictOutcomeV1::Conflict,
            ConflictOutcomeV1::NoConflict,
            ConflictOutcomeV1::Censored,
            ConflictOutcomeV1::Unknown,
        ];
        for kind in KINDS {
            let mut eligible = 0u64;
            for prediction in self.predictions.values() {
                if prediction.kind == kind {
                    eligible = eligible.saturating_add(1);
                }
            }
            let mut linked = 0u64;
            let mut censored = 0u64;
            let mut unknown = 0u64;
            let mut true_positive = 0u64;
            let mut false_positive = 0u64;
            let mut false_negative = 0u64;
            let mut by_outcome: BTreeMap<&'static str, u64> = BTreeMap::new();
            for (reference, outcome) in &self.outcomes {
                if outcome.kind != kind {
                    continue;
                }
                let Some(prediction) = self.predictions.get(reference) else {
                    continue;
                };
                if prediction.kind != kind {
                    continue;
                }
                let entry = by_outcome.entry(outcome_key(outcome.outcome)).or_insert(0);
                *entry = entry.saturating_add(1);
                match outcome.outcome {
                    ConflictOutcomeV1::Censored => {
                        censored = censored.saturating_add(1);
                        continue;
                    }
                    ConflictOutcomeV1::Unknown => {
                        unknown = unknown.saturating_add(1);
                        continue;
                    }
                    _ => {}
                }
                match (prediction.prediction, outcome.outcome) {
                    (ConflictPredictionV1::Conflict, ConflictOutcomeV1::Conflict) => {
                        linked = linked.saturating_add(1);
                        true_positive = true_positive.saturating_add(1);
                    }
                    (ConflictPredictionV1::Conflict, ConflictOutcomeV1::NoConflict) => {
                        linked = linked.saturating_add(1);
                        false_positive = false_positive.saturating_add(1);
                    }
                    (ConflictPredictionV1::NoConflict, ConflictOutcomeV1::Conflict) => {
                        linked = linked.saturating_add(1);
                        false_negative = false_negative.saturating_add(1);
                    }
                    (ConflictPredictionV1::NoConflict, ConflictOutcomeV1::NoConflict) => {
                        linked = linked.saturating_add(1);
                    }
                    // An abstained or unknown prediction is observed but is
                    // not a decision, so it links without entering either
                    // confusion-matrix denominator.
                    _ => linked = linked.saturating_add(1),
                }
            }
            // A prediction that never received an independent outcome inside
            // the horizon is censored, not a silent success.
            let accounted = linked.saturating_add(censored).saturating_add(unknown);
            censored = censored.saturating_add(eligible.saturating_sub(accounted));
            let coverage = MetricCoverageV1 {
                eligible: context.complete.then_some(eligible),
                observed: linked,
                completed: linked,
                censored,
                unknown,
                excluded: 0,
                state: distribution_state(context.complete, eligible, linked),
            };
            let count_reason = count_refusal(context.complete, eligible);
            for outcome in OUTCOMES {
                let total = by_outcome.get(outcome_key(outcome)).copied().unwrap_or(0);
                out.push(measurement(MeasurementInput {
                    metric: "work_conflict_prediction_total",
                    unit: "events",
                    denominator: "linked_conflict_predictions",
                    evidence_class: MetricEvidenceClassV1::Association,
                    dimensions: vec![
                        ExecutionTopologyDimensionV1::ConflictKind(kind.into()),
                        ExecutionTopologyDimensionV1::ConflictOutcome(outcome.into()),
                    ],
                    coverage: coverage.clone(),
                    value: if count_reason.is_some() {
                        None
                    } else {
                        Some(as_f64(total))
                    },
                    unavailable: count_reason,
                    context,
                }));
            }
            let rate_reason = conflict_refusal(context.complete, eligible, linked, censored);
            let precision_denominator = true_positive.saturating_add(false_positive);
            let precision_reason = rate_reason.or((precision_denominator == 0)
                .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement(MeasurementInput {
                metric: "work_conflict_prediction_precision",
                unit: "ratio",
                denominator: "predicted_conflicts_with_outcome",
                evidence_class: MetricEvidenceClassV1::Association,
                dimensions: vec![ExecutionTopologyDimensionV1::ConflictKind(kind.into())],
                coverage: coverage.clone(),
                value: if precision_reason.is_some() {
                    None
                } else {
                    ratio(true_positive, precision_denominator)
                },
                unavailable: precision_reason,
                context,
            }));
            let recall_denominator = true_positive.saturating_add(false_negative);
            let recall_reason = rate_reason.or((recall_denominator == 0)
                .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement(MeasurementInput {
                metric: "work_conflict_prediction_recall",
                unit: "ratio",
                denominator: "observed_conflicts_with_prediction",
                evidence_class: MetricEvidenceClassV1::Association,
                dimensions: vec![ExecutionTopologyDimensionV1::ConflictKind(kind.into())],
                coverage: coverage.clone(),
                value: if recall_reason.is_some() {
                    None
                } else {
                    ratio(true_positive, recall_denominator)
                },
                unavailable: recall_reason,
                context,
            }));
        }
    }
}
