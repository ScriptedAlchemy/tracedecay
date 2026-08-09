use std::collections::BTreeMap;

use super::super::support::{
    MeasurementInput, as_f64, conflict_refusal, count_refusal, count_state, distribution_refusal,
    distribution_state, measurement, measurement_with_local_support, ratio,
};
use super::super::{
    ALL_QUANTITY_UNITS_V1, ALL_WIDTH_BUCKETS_V1, ExecutionConcurrencyPhaseV1,
    ExecutionFanoutPhaseV1, ExecutionMetricUnavailableV1, ExecutionTopologyDimensionV1,
    ExecutionTopologyMeasurementV1,
};
use super::{
    ConflictOutcomeRowV1, ConflictPredictionRowV1, DuplicateRowV1, ExecutionTopologyEvidenceV1,
    ExecutionTopologyRollupStateErrorV1, ProjectionContext, TopologySampleV1,
};
use crate::observability::{MetricCoverageV1, MetricEvidenceClassV1};
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1, CoverageStateV1,
    DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
};
const WIDTH_BUCKET_COUNT_V1: usize = 9;
const PHASE_COUNT_V1: usize = 5;
const QUANTITY_UNIT_COUNT_V1: usize = 5;
const DUPLICATE_KIND_COUNT_V1: usize = 4;
const CONFLICT_KIND_COUNT_V1: usize = 3;
const CONFLICT_OUTCOME_COUNT_V1: usize = 4;
const DUPLICATE_EFFECT_OUTCOME_COUNT_V1: usize = 3;
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyCapacityRollupV1 {
    topology: TopologyCapacityRollupV1,
    duplicate: DuplicateCapacityRollupV1,
    conflict: [ConflictCapacityRollupV1; CONFLICT_KIND_COUNT_V1],
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TopologyCapacityRollupV1 {
    eligible: u64,
    duration_observed: u64,
    #[serde(default)]
    duration_observed_by_phase_bucket: [[u64; WIDTH_BUCKET_COUNT_V1]; PHASE_COUNT_V1],
    duration_micros_by_phase_bucket: [[u64; WIDTH_BUCKET_COUNT_V1]; PHASE_COUNT_V1],
    fanout_by_phase_bucket: [[u64; WIDTH_BUCKET_COUNT_V1]; PHASE_COUNT_V1],
    useful_attempt_micros: u64,
    admitted_attempt_micros: u64,
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DuplicateCapacityRollupV1 {
    eligible: u64,
    adjudicated: u64,
    censored: u64,
    unknown: u64,
    duplicate_quantity_by_kind_unit: [[u64; QUANTITY_UNIT_COUNT_V1]; DUPLICATE_KIND_COUNT_V1],
    #[serde(default)]
    duplicate_observations_by_kind_unit: [[u64; QUANTITY_UNIT_COUNT_V1]; DUPLICATE_KIND_COUNT_V1],
    duplicate_quantity_by_unit: [u64; QUANTITY_UNIT_COUNT_V1],
    #[serde(default)]
    population_observations_by_unit: [u64; QUANTITY_UNIT_COUNT_V1],
    population_quantity_by_unit: [u64; QUANTITY_UNIT_COUNT_V1],
    effect_eligible: u64,
    effect_observed: u64,
    effect_unknown: u64,
    effect_excluded: u64,
    effects_by_outcome: [u64; DUPLICATE_EFFECT_OUTCOME_COUNT_V1],
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ConflictCapacityRollupV1 {
    eligible: u64,
    linked: u64,
    censored: u64,
    unknown: u64,
    true_positive: u64,
    false_positive: u64,
    false_negative: u64,
    outcomes: [u64; CONFLICT_OUTCOME_COUNT_V1],
}
impl ExecutionTopologyEvidenceV1 {
    pub(in crate::execution_topology_metrics) fn reduce_capacity_rollup(
        &self,
    ) -> Result<ExecutionTopologyCapacityRollupV1, ExecutionTopologyRollupStateErrorV1> {
        Ok(self.reduce_topology_capacity())
    }

    fn reduce_topology_capacity(&self) -> ExecutionTopologyCapacityRollupV1 {
        let mut capacity = ExecutionTopologyCapacityRollupV1::default();
        for sample in &self.topology {
            capacity.absorb_topology(sample);
        }
        capacity
    }
}

impl ExecutionTopologyCapacityRollupV1 {
    pub(in crate::execution_topology_metrics) fn validate(
        &self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        if self.topology.fanout_by_phase_bucket.iter().any(|buckets| {
            buckets.iter().copied().fold(0u64, u64::saturating_add) != self.topology.eligible
        }) || self.topology.duration_observed > self.topology.eligible
            || self.topology.useful_attempt_micros > self.topology.admitted_attempt_micros
            || self
                .topology
                .duration_observed_by_phase_bucket
                .iter()
                .any(|buckets| {
                    buckets.iter().copied().fold(0u64, u64::saturating_add)
                        != self.topology.duration_observed
                })
            || self
                .duplicate
                .duplicate_observations_by_kind_unit
                .iter()
                .flatten()
                .copied()
                .any(|support| support > self.duplicate.adjudicated)
            || self
                .duplicate
                .population_observations_by_unit
                .iter()
                .copied()
                .any(|support| support > self.duplicate.adjudicated)
            || (0..QUANTITY_UNIT_COUNT_V1).any(|unit| {
                checked_sum(
                    self.duplicate
                        .duplicate_quantity_by_kind_unit
                        .iter()
                        .map(|quantities| quantities[unit]),
                ) != Some(self.duplicate.duplicate_quantity_by_unit[unit])
                    || self.duplicate.duplicate_quantity_by_unit[unit]
                        > self.duplicate.population_quantity_by_unit[unit]
                    || checked_sum(
                        self.duplicate
                            .duplicate_observations_by_kind_unit
                            .iter()
                            .map(|observations| observations[unit]),
                    )
                    .is_none_or(|duplicate_observations| {
                        duplicate_observations
                            > self.duplicate.population_observations_by_unit[unit]
                    })
            })
            || self
                .duplicate
                .adjudicated
                .saturating_add(self.duplicate.censored)
                .saturating_add(self.duplicate.unknown)
                != self.duplicate.eligible
            || self
                .duplicate
                .effect_eligible
                .saturating_add(self.duplicate.effect_excluded)
                != self.duplicate.eligible
            || self
                .duplicate
                .effect_observed
                .saturating_add(self.duplicate.effect_unknown)
                != self.duplicate.effect_eligible
            || self
                .duplicate
                .effects_by_outcome
                .iter()
                .copied()
                .fold(0u64, u64::saturating_add)
                != self.duplicate.effect_observed
            || self.conflict.iter().any(|stats| {
                stats
                    .linked
                    .saturating_add(stats.censored)
                    .saturating_add(stats.unknown)
                    != stats.eligible
                    || checked_sum(stats.outcomes) != Some(stats.linked)
                    || stats.true_positive.saturating_add(stats.false_negative)
                        != stats.outcomes[conflict_outcome_index(ConflictOutcomeV1::Conflict)]
                    || stats.false_positive
                        > stats.outcomes[conflict_outcome_index(ConflictOutcomeV1::NoConflict)]
                    || stats.true_positive.saturating_add(stats.false_positive) > stats.linked
                    || stats.true_positive.saturating_add(stats.false_negative) > stats.linked
            })
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn merge(
        &mut self,
        other: Self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        add_topology(&mut self.topology, other.topology);
        add_duplicate(&mut self.duplicate, other.duplicate);
        for (target, incoming) in self.conflict.iter_mut().zip(other.conflict) {
            add_conflict(target, incoming);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn project(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        self.project_concurrency_width(context, out);
        self.project_useful_ratio(context, out);
        self.project_fanout_width(context, out);
        self.project_duplicate_effort(context, out);
        self.project_duplicate_effects(context, out);
        self.project_conflict(context, out);
    }

    fn absorb_topology(&mut self, sample: &TopologySampleV1) {
        self.topology.eligible = self.topology.eligible.saturating_add(1);
        for (phase, width) in sample.widths.iter().enumerate() {
            let bucket = width_bucket_index(*width);
            self.topology.fanout_by_phase_bucket[phase][bucket] =
                self.topology.fanout_by_phase_bucket[phase][bucket].saturating_add(1);
        }
        let Some(duration) = sample.interval_micros else {
            return;
        };
        if sample.coverage != CoverageStateV1::Known {
            return;
        }
        self.topology.duration_observed = self.topology.duration_observed.saturating_add(1);
        for (phase, width) in sample.widths.iter().enumerate() {
            let bucket = width_bucket_index(*width);
            self.topology.duration_observed_by_phase_bucket[phase][bucket] =
                self.topology.duration_observed_by_phase_bucket[phase][bucket].saturating_add(1);
            self.topology.duration_micros_by_phase_bucket[phase][bucket] =
                self.topology.duration_micros_by_phase_bucket[phase][bucket]
                    .saturating_add(duration);
        }
        self.topology.useful_attempt_micros = self
            .topology
            .useful_attempt_micros
            .saturating_add(u64::from(sample.widths[4]).saturating_mul(duration));
        self.topology.admitted_attempt_micros = self
            .topology
            .admitted_attempt_micros
            .saturating_add(u64::from(sample.widths[2]).saturating_mul(duration));
    }

    pub(super) fn absorb_duplicate_rows(
        &mut self,
        rows: &BTreeMap<String, (Option<DuplicateRowV1>, i64)>,
    ) {
        for (row, _) in rows.values() {
            self.absorb_duplicate_row(*row);
        }
    }

    fn absorb_duplicate_row(&mut self, row: Option<DuplicateRowV1>) {
        self.duplicate.eligible = self.duplicate.eligible.saturating_add(1);
        let Some(row) = row else {
            self.duplicate.unknown = self.duplicate.unknown.saturating_add(1);
            self.duplicate.effect_eligible = self.duplicate.effect_eligible.saturating_add(1);
            self.duplicate.effect_unknown = self.duplicate.effect_unknown.saturating_add(1);
            return;
        };
        if row.coverage != CoverageStateV1::Known {
            self.duplicate.unknown = self.duplicate.unknown.saturating_add(1);
        } else {
            match row.kind {
                DuplicateEffortKindV1::Censored => {
                    self.duplicate.censored = self.duplicate.censored.saturating_add(1)
                }
                DuplicateEffortKindV1::Unknown => {
                    self.duplicate.unknown = self.duplicate.unknown.saturating_add(1)
                }
                _ => self.duplicate.adjudicated = self.duplicate.adjudicated.saturating_add(1),
            }
        }
        if row.effect_outcome == DuplicateEffectOutcomeV1::NotApplicable {
            self.duplicate.effect_excluded = self.duplicate.effect_excluded.saturating_add(1);
        } else {
            self.duplicate.effect_eligible = self.duplicate.effect_eligible.saturating_add(1);
            if row.coverage == CoverageStateV1::Known {
                self.duplicate.effect_observed = self.duplicate.effect_observed.saturating_add(1);
                self.duplicate.effects_by_outcome[duplicate_effect_index(row.effect_outcome)] =
                    self.duplicate.effects_by_outcome[duplicate_effect_index(row.effect_outcome)]
                        .saturating_add(1);
            } else {
                self.duplicate.effect_unknown = self.duplicate.effect_unknown.saturating_add(1);
            }
        }
        if row.coverage != CoverageStateV1::Known {
            return;
        }
        let duplicate_kind = duplicate_kind_index(row.kind);
        for (unit, quantity) in row.quantities.into_iter().enumerate() {
            let Some(quantity) = quantity else {
                continue;
            };
            match row.kind {
                DuplicateEffortKindV1::Censored | DuplicateEffortKindV1::Unknown => {}
                DuplicateEffortKindV1::NotDuplicate => {
                    self.duplicate.population_observations_by_unit[unit] =
                        self.duplicate.population_observations_by_unit[unit].saturating_add(1);
                    self.duplicate.population_quantity_by_unit[unit] =
                        self.duplicate.population_quantity_by_unit[unit].saturating_add(quantity);
                }
                _ => {
                    self.duplicate.duplicate_observations_by_kind_unit[duplicate_kind][unit] =
                        self.duplicate.duplicate_observations_by_kind_unit[duplicate_kind][unit]
                            .saturating_add(1);
                    self.duplicate.duplicate_quantity_by_kind_unit[duplicate_kind][unit] =
                        self.duplicate.duplicate_quantity_by_kind_unit[duplicate_kind][unit]
                            .saturating_add(quantity);
                    self.duplicate.duplicate_quantity_by_unit[unit] =
                        self.duplicate.duplicate_quantity_by_unit[unit].saturating_add(quantity);
                    self.duplicate.population_observations_by_unit[unit] =
                        self.duplicate.population_observations_by_unit[unit].saturating_add(1);
                    self.duplicate.population_quantity_by_unit[unit] =
                        self.duplicate.population_quantity_by_unit[unit].saturating_add(quantity);
                }
            }
        }
    }

    pub(super) fn absorb_conflict_rows(
        &mut self,
        predictions: &BTreeMap<String, ConflictPredictionRowV1>,
        outcomes: &BTreeMap<String, ConflictOutcomeRowV1>,
    ) {
        for prediction in predictions.values() {
            self.conflict[conflict_kind_index(prediction.kind)].eligible = self.conflict
                [conflict_kind_index(prediction.kind)]
            .eligible
            .saturating_add(1);
        }
        for (reference, outcome) in outcomes {
            let Some(prediction) = predictions.get(reference) else {
                continue;
            };
            if prediction.kind != outcome.kind {
                continue;
            }
            let stats = &mut self.conflict[conflict_kind_index(outcome.kind)];
            if prediction.coverage != CoverageStateV1::Known
                || outcome.coverage != CoverageStateV1::Known
            {
                if outcome.outcome == ConflictOutcomeV1::Censored {
                    stats.censored = stats.censored.saturating_add(1);
                } else {
                    stats.unknown = stats.unknown.saturating_add(1);
                }
                continue;
            }
            match outcome.outcome {
                ConflictOutcomeV1::Censored => {
                    stats.censored = stats.censored.saturating_add(1);
                    continue;
                }
                ConflictOutcomeV1::Unknown => {
                    stats.unknown = stats.unknown.saturating_add(1);
                    continue;
                }
                _ => {
                    stats.linked = stats.linked.saturating_add(1);
                    stats.outcomes[conflict_outcome_index(outcome.outcome)] =
                        stats.outcomes[conflict_outcome_index(outcome.outcome)].saturating_add(1);
                }
            }
            match (prediction.prediction, outcome.outcome) {
                (ConflictPredictionV1::Conflict, ConflictOutcomeV1::Conflict) => {
                    stats.true_positive = stats.true_positive.saturating_add(1)
                }
                (ConflictPredictionV1::Conflict, ConflictOutcomeV1::NoConflict) => {
                    stats.false_positive = stats.false_positive.saturating_add(1)
                }
                (ConflictPredictionV1::NoConflict, ConflictOutcomeV1::Conflict) => {
                    stats.false_negative = stats.false_negative.saturating_add(1)
                }
                _ => {}
            }
        }
        for stats in &mut self.conflict {
            let accounted = stats
                .linked
                .saturating_add(stats.censored)
                .saturating_add(stats.unknown);
            stats.censored = stats
                .censored
                .saturating_add(stats.eligible.saturating_sub(accounted));
        }
    }

    fn project_concurrency_width(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let coverage = duration_coverage(context, &self.topology);
        let refusal = distribution_refusal(
            context.complete,
            self.topology.eligible,
            self.topology.duration_observed,
        );
        for (phase_index, phase) in concurrency_phases().into_iter().enumerate() {
            if let Some(reason) = refusal {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_execution_concurrency_width",
                        unit: "microseconds",
                        denominator: "duration_weighted_topology_samples",
                        evidence_class: MetricEvidenceClassV1::Measurement,
                        dimensions: vec![ExecutionTopologyDimensionV1::ConcurrencyPhase(phase)],
                        coverage: coverage.clone(),
                        value: None,
                        unavailable: Some(reason),
                        context,
                    },
                    self.topology.duration_observed,
                ));
                continue;
            }
            for (bucket_index, bucket) in ALL_WIDTH_BUCKETS_V1.into_iter().enumerate() {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_execution_concurrency_width",
                        unit: "microseconds",
                        denominator: "duration_weighted_topology_samples",
                        evidence_class: MetricEvidenceClassV1::Measurement,
                        dimensions: vec![
                            ExecutionTopologyDimensionV1::ConcurrencyPhase(phase),
                            ExecutionTopologyDimensionV1::WidthBucket(bucket),
                        ],
                        coverage: coverage.clone(),
                        value: Some(as_f64(
                            self.topology.duration_micros_by_phase_bucket[phase_index]
                                [bucket_index],
                        )),
                        unavailable: None,
                        context,
                    },
                    self.topology.duration_observed_by_phase_bucket[phase_index][bucket_index],
                ));
            }
        }
    }

    fn project_useful_ratio(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let coverage = duration_coverage(context, &self.topology);
        let refusal = distribution_refusal(
            context.complete,
            self.topology.eligible,
            self.topology.duration_observed,
        )
        .or((self.topology.admitted_attempt_micros == 0)
            .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
        out.push(measurement(MeasurementInput {
            metric: "work_execution_useful_concurrency_ratio",
            unit: "ratio",
            denominator: "admitted_attempt_micros",
            evidence_class: MetricEvidenceClassV1::Measurement,
            dimensions: Vec::new(),
            coverage,
            value: refusal
                .is_none()
                .then(|| {
                    ratio(
                        self.topology.useful_attempt_micros,
                        self.topology.admitted_attempt_micros,
                    )
                })
                .flatten(),
            unavailable: refusal,
            context,
        }));
    }

    fn project_fanout_width(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let eligible = self.topology.eligible;
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
        for (phase_index, phase) in fanout_phases().into_iter().enumerate() {
            if let Some(reason) = refusal {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_execution_fanout_width",
                        unit: "events",
                        denominator: "topology_samples",
                        evidence_class: MetricEvidenceClassV1::Measurement,
                        dimensions: vec![ExecutionTopologyDimensionV1::FanoutPhase(phase)],
                        coverage: coverage.clone(),
                        value: None,
                        unavailable: Some(reason),
                        context,
                    },
                    self.topology.eligible,
                ));
                continue;
            }
            for (bucket_index, bucket) in ALL_WIDTH_BUCKETS_V1.into_iter().enumerate() {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_execution_fanout_width",
                        unit: "events",
                        denominator: "topology_samples",
                        evidence_class: MetricEvidenceClassV1::Measurement,
                        dimensions: vec![
                            ExecutionTopologyDimensionV1::FanoutPhase(phase),
                            ExecutionTopologyDimensionV1::WidthBucket(bucket),
                        ],
                        coverage: coverage.clone(),
                        value: Some(as_f64(
                            self.topology.fanout_by_phase_bucket[phase_index][bucket_index],
                        )),
                        unavailable: None,
                        context,
                    },
                    self.topology.fanout_by_phase_bucket[phase_index][bucket_index],
                ));
            }
        }
    }

    fn project_duplicate_effort(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(self.duplicate.eligible),
            observed: self.duplicate.adjudicated,
            completed: self.duplicate.adjudicated,
            censored: self.duplicate.censored,
            unknown: self.duplicate.unknown,
            excluded: 0,
            state: distribution_state(
                context.complete,
                self.duplicate.eligible,
                self.duplicate.adjudicated,
            ),
        };
        let refusal = count_refusal(context.complete, self.duplicate.eligible);
        for (unit_index, unit) in ALL_QUANTITY_UNITS_V1.into_iter().enumerate() {
            for (kind_index, kind) in duplicate_kinds().into_iter().enumerate() {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_duplicate_effort_total",
                        unit: unit.wire_unit(),
                        denominator: "adjudicated_duplicate_relations",
                        evidence_class: MetricEvidenceClassV1::Measurement,
                        dimensions: vec![
                            ExecutionTopologyDimensionV1::DuplicateKind(kind.into()),
                            ExecutionTopologyDimensionV1::Unit(unit),
                        ],
                        coverage: coverage.clone(),
                        value: refusal.is_none().then(|| {
                            as_f64(
                                self.duplicate.duplicate_quantity_by_kind_unit[kind_index]
                                    [unit_index],
                            )
                        }),
                        unavailable: refusal,
                        context,
                    },
                    self.duplicate.duplicate_observations_by_kind_unit[kind_index][unit_index],
                ));
            }
            let population = self.duplicate.population_quantity_by_unit[unit_index];
            let ratio_refusal = refusal
                .or((population == 0).then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement_with_local_support(
                MeasurementInput {
                    metric: "work_duplicate_effort_ratio",
                    unit: "ratio",
                    denominator: "adjudicated_effort_quantity",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![ExecutionTopologyDimensionV1::Unit(unit)],
                    coverage: coverage.clone(),
                    value: ratio_refusal
                        .is_none()
                        .then(|| {
                            ratio(
                                self.duplicate.duplicate_quantity_by_unit[unit_index],
                                population,
                            )
                        })
                        .flatten(),
                    unavailable: ratio_refusal,
                    context,
                },
                self.duplicate.population_observations_by_unit[unit_index],
            ));
        }
    }

    fn project_duplicate_effects(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        let coverage = MetricCoverageV1 {
            eligible: context.complete.then_some(self.duplicate.effect_eligible),
            observed: self.duplicate.effect_observed,
            completed: self.duplicate.effect_observed,
            censored: 0,
            unknown: self.duplicate.effect_unknown,
            excluded: self.duplicate.effect_excluded,
            state: distribution_state(
                context.complete,
                self.duplicate.effect_eligible,
                self.duplicate.effect_observed,
            ),
        };
        let refusal = count_refusal(context.complete, self.duplicate.effect_eligible);
        for (index, outcome) in duplicate_effect_outcomes().into_iter().enumerate() {
            out.push(measurement_with_local_support(
                MeasurementInput {
                    metric: "work_duplicate_effects_total",
                    unit: "events",
                    denominator: "observed_duplicate_effects",
                    evidence_class: MetricEvidenceClassV1::Measurement,
                    dimensions: vec![ExecutionTopologyDimensionV1::DuplicateOutcome(
                        outcome.into(),
                    )],
                    coverage: coverage.clone(),
                    value: refusal
                        .is_none()
                        .then(|| as_f64(self.duplicate.effects_by_outcome[index])),
                    unavailable: refusal,
                    context,
                },
                self.duplicate.effects_by_outcome[index],
            ));
        }
    }

    fn project_conflict(
        &self,
        context: &ProjectionContext,
        out: &mut Vec<ExecutionTopologyMeasurementV1>,
    ) {
        for (kind_index, kind) in conflict_kinds().into_iter().enumerate() {
            let stats = &self.conflict[kind_index];
            let coverage = MetricCoverageV1 {
                eligible: context.complete.then_some(stats.eligible),
                observed: stats.linked,
                completed: stats.linked,
                censored: stats.censored,
                unknown: stats.unknown,
                excluded: 0,
                state: distribution_state(context.complete, stats.eligible, stats.linked),
            };
            let count_reason = count_refusal(context.complete, stats.eligible);
            for (outcome_index, outcome) in conflict_outcomes().into_iter().enumerate() {
                out.push(measurement_with_local_support(
                    MeasurementInput {
                        metric: "work_conflict_prediction_total",
                        unit: "events",
                        denominator: "linked_conflict_predictions",
                        evidence_class: MetricEvidenceClassV1::Association,
                        dimensions: vec![
                            ExecutionTopologyDimensionV1::ConflictKind(kind.into()),
                            ExecutionTopologyDimensionV1::ConflictOutcome(outcome.into()),
                        ],
                        coverage: coverage.clone(),
                        value: count_reason
                            .is_none()
                            .then(|| as_f64(stats.outcomes[outcome_index])),
                        unavailable: count_reason,
                        context,
                    },
                    stats.outcomes[outcome_index],
                ));
            }
            let rate_reason = conflict_refusal(
                context.complete,
                stats.eligible,
                stats.linked,
                stats.censored,
            );
            let precision_denominator = stats.true_positive.saturating_add(stats.false_positive);
            let precision_reason = rate_reason.or((precision_denominator == 0)
                .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement_with_local_support(
                MeasurementInput {
                    metric: "work_conflict_prediction_precision",
                    unit: "ratio",
                    denominator: "predicted_conflicts_with_outcome",
                    evidence_class: MetricEvidenceClassV1::Association,
                    dimensions: vec![ExecutionTopologyDimensionV1::ConflictKind(kind.into())],
                    coverage: coverage.clone(),
                    value: precision_reason
                        .is_none()
                        .then(|| ratio(stats.true_positive, precision_denominator))
                        .flatten(),
                    unavailable: precision_reason,
                    context,
                },
                precision_denominator,
            ));
            let recall_denominator = stats.true_positive.saturating_add(stats.false_negative);
            let recall_reason = rate_reason.or((recall_denominator == 0)
                .then_some(ExecutionMetricUnavailableV1::NoEligibleEvidence));
            out.push(measurement_with_local_support(
                MeasurementInput {
                    metric: "work_conflict_prediction_recall",
                    unit: "ratio",
                    denominator: "observed_conflicts_with_prediction",
                    evidence_class: MetricEvidenceClassV1::Association,
                    dimensions: vec![ExecutionTopologyDimensionV1::ConflictKind(kind.into())],
                    coverage,
                    value: recall_reason
                        .is_none()
                        .then(|| ratio(stats.true_positive, recall_denominator))
                        .flatten(),
                    unavailable: recall_reason,
                    context,
                },
                recall_denominator,
            ));
        }
    }
}

fn add_topology(target: &mut TopologyCapacityRollupV1, incoming: TopologyCapacityRollupV1) {
    target.eligible = target.eligible.saturating_add(incoming.eligible);
    target.duration_observed = target
        .duration_observed
        .saturating_add(incoming.duration_observed);
    target.useful_attempt_micros = target
        .useful_attempt_micros
        .saturating_add(incoming.useful_attempt_micros);
    target.admitted_attempt_micros = target
        .admitted_attempt_micros
        .saturating_add(incoming.admitted_attempt_micros);
    add_matrix(
        &mut target.duration_observed_by_phase_bucket,
        incoming.duration_observed_by_phase_bucket,
    );
    add_matrix(
        &mut target.duration_micros_by_phase_bucket,
        incoming.duration_micros_by_phase_bucket,
    );
    add_matrix(
        &mut target.fanout_by_phase_bucket,
        incoming.fanout_by_phase_bucket,
    );
}

fn add_duplicate(target: &mut DuplicateCapacityRollupV1, incoming: DuplicateCapacityRollupV1) {
    target.eligible = target.eligible.saturating_add(incoming.eligible);
    target.adjudicated = target.adjudicated.saturating_add(incoming.adjudicated);
    target.censored = target.censored.saturating_add(incoming.censored);
    target.unknown = target.unknown.saturating_add(incoming.unknown);
    target.effect_eligible = target
        .effect_eligible
        .saturating_add(incoming.effect_eligible);
    target.effect_observed = target
        .effect_observed
        .saturating_add(incoming.effect_observed);
    target.effect_unknown = target
        .effect_unknown
        .saturating_add(incoming.effect_unknown);
    target.effect_excluded = target
        .effect_excluded
        .saturating_add(incoming.effect_excluded);
    add_array(
        &mut target.duplicate_quantity_by_unit,
        incoming.duplicate_quantity_by_unit,
    );
    add_array(
        &mut target.population_quantity_by_unit,
        incoming.population_quantity_by_unit,
    );
    add_array(&mut target.effects_by_outcome, incoming.effects_by_outcome);
    add_matrix(
        &mut target.duplicate_observations_by_kind_unit,
        incoming.duplicate_observations_by_kind_unit,
    );
    add_array(
        &mut target.population_observations_by_unit,
        incoming.population_observations_by_unit,
    );
    add_matrix(
        &mut target.duplicate_quantity_by_kind_unit,
        incoming.duplicate_quantity_by_kind_unit,
    );
}

fn add_conflict(target: &mut ConflictCapacityRollupV1, incoming: ConflictCapacityRollupV1) {
    target.eligible = target.eligible.saturating_add(incoming.eligible);
    target.linked = target.linked.saturating_add(incoming.linked);
    target.censored = target.censored.saturating_add(incoming.censored);
    target.unknown = target.unknown.saturating_add(incoming.unknown);
    target.true_positive = target.true_positive.saturating_add(incoming.true_positive);
    target.false_positive = target
        .false_positive
        .saturating_add(incoming.false_positive);
    target.false_negative = target
        .false_negative
        .saturating_add(incoming.false_negative);
    add_array(&mut target.outcomes, incoming.outcomes);
}

fn add_array<const N: usize>(target: &mut [u64; N], incoming: [u64; N]) {
    for (target, incoming) in target.iter_mut().zip(incoming) {
        *target = target.saturating_add(incoming);
    }
}

fn checked_sum(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0u64, |total, value| total.checked_add(value))
}

fn add_matrix<const ROWS: usize, const COLUMNS: usize>(
    target: &mut [[u64; COLUMNS]; ROWS],
    incoming: [[u64; COLUMNS]; ROWS],
) {
    for (target, incoming) in target.iter_mut().zip(incoming) {
        add_array(target, incoming);
    }
}

fn duration_coverage(
    context: &ProjectionContext,
    topology: &TopologyCapacityRollupV1,
) -> MetricCoverageV1 {
    MetricCoverageV1 {
        eligible: context.complete.then_some(topology.eligible),
        observed: topology.duration_observed,
        completed: topology.duration_observed,
        censored: topology.eligible.saturating_sub(topology.duration_observed),
        unknown: 0,
        excluded: 0,
        state: distribution_state(
            context.complete,
            topology.eligible,
            topology.duration_observed,
        ),
    }
}

const fn width_bucket_index(width: u16) -> usize {
    match width {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=4 => 3,
        5..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        33..=64 => 7,
        _ => 8,
    }
}

const fn duplicate_kind_index(kind: DuplicateEffortKindV1) -> usize {
    match kind {
        DuplicateEffortKindV1::ExactDuplicate => 0,
        DuplicateEffortKindV1::SupersededOverlap => 1,
        DuplicateEffortKindV1::RepeatedInvestigation => 2,
        DuplicateEffortKindV1::DuplicateEffect => 3,
        DuplicateEffortKindV1::NotDuplicate
        | DuplicateEffortKindV1::Censored
        | DuplicateEffortKindV1::Unknown => 0,
    }
}

const fn duplicate_effect_index(outcome: DuplicateEffectOutcomeV1) -> usize {
    match outcome {
        DuplicateEffectOutcomeV1::Prevented => 0,
        DuplicateEffectOutcomeV1::Committed => 1,
        DuplicateEffectOutcomeV1::Unknown | DuplicateEffectOutcomeV1::NotApplicable => 2,
    }
}

const fn conflict_kind_index(kind: ConflictKindV1) -> usize {
    match kind {
        ConflictKindV1::Mechanical => 0,
        ConflictKindV1::Semantic => 1,
        ConflictKindV1::Combined => 2,
    }
}

const fn conflict_outcome_index(outcome: ConflictOutcomeV1) -> usize {
    match outcome {
        ConflictOutcomeV1::Conflict => 0,
        ConflictOutcomeV1::NoConflict => 1,
        ConflictOutcomeV1::Censored => 2,
        ConflictOutcomeV1::Unknown => 3,
    }
}

const fn concurrency_phases() -> [ExecutionConcurrencyPhaseV1; PHASE_COUNT_V1] {
    [
        ExecutionConcurrencyPhaseV1::Requested,
        ExecutionConcurrencyPhaseV1::Accepted,
        ExecutionConcurrencyPhaseV1::Admitted,
        ExecutionConcurrencyPhaseV1::Active,
        ExecutionConcurrencyPhaseV1::Useful,
    ]
}

const fn fanout_phases() -> [ExecutionFanoutPhaseV1; PHASE_COUNT_V1] {
    [
        ExecutionFanoutPhaseV1::Requested,
        ExecutionFanoutPhaseV1::Accepted,
        ExecutionFanoutPhaseV1::Admitted,
        ExecutionFanoutPhaseV1::PeakActive,
        ExecutionFanoutPhaseV1::Useful,
    ]
}

const fn duplicate_kinds() -> [DuplicateEffortKindV1; DUPLICATE_KIND_COUNT_V1] {
    [
        DuplicateEffortKindV1::ExactDuplicate,
        DuplicateEffortKindV1::SupersededOverlap,
        DuplicateEffortKindV1::RepeatedInvestigation,
        DuplicateEffortKindV1::DuplicateEffect,
    ]
}

const fn duplicate_effect_outcomes() -> [DuplicateEffectOutcomeV1; DUPLICATE_EFFECT_OUTCOME_COUNT_V1]
{
    [
        DuplicateEffectOutcomeV1::Prevented,
        DuplicateEffectOutcomeV1::Committed,
        DuplicateEffectOutcomeV1::Unknown,
    ]
}

const fn conflict_kinds() -> [ConflictKindV1; CONFLICT_KIND_COUNT_V1] {
    [
        ConflictKindV1::Mechanical,
        ConflictKindV1::Semantic,
        ConflictKindV1::Combined,
    ]
}

const fn conflict_outcomes() -> [ConflictOutcomeV1; CONFLICT_OUTCOME_COUNT_V1] {
    [
        ConflictOutcomeV1::Conflict,
        ConflictOutcomeV1::NoConflict,
        ConflictOutcomeV1::Censored,
        ConflictOutcomeV1::Unknown,
    ]
}
