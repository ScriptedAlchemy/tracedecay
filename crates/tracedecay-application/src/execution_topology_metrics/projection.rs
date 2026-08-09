//! Event ingest and the per-family projection entry point.
//!
//! Classified rows are reduced into bounded capacity and lifecycle rollups;
//! event-scale joins never cross that reduction boundary.
pub(super) mod capacity_corrections;
pub(super) mod capacity_rollup;
pub(super) mod lifecycle_rollup;
pub(super) mod lifecycle_rollup_projection;
pub(super) mod page_projection;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BlockedCauseV1, ConflictKindV1, ConflictOutcomeV1, ConflictPredictionV1, CoverageStateV1,
    DeliverySurfaceFamilyV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1, DurationBucketV1,
    GitHubStackCapabilityV1, IntegrationOperationKindV1, IntegrationPhaseV1, IntegrationResultV1,
    IntervalStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, RerunCauseV1, RerunSourceV1,
    StackDriftKindV1, WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1, canonical_sha256,
};

use crate::observability::ObservabilityHorizonV1;

use super::support::bounded_interval;

/// Cross-cutting producer-loss receipt queried alongside topology families.
pub(super) const TELEMETRY_DROP_EVENT_KIND_V1: &str = "telemetry.drop.observed.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::execution_topology_metrics) struct ProjectionContext {
    pub(in crate::execution_topology_metrics) horizon: ObservabilityHorizonV1,
    pub(in crate::execution_topology_metrics) watermark: String,
    /// The whole family read with `Known` coverage. Every ratio, rate, and
    /// distribution below refuses without it, because a partial event
    /// population silently understates every denominator.
    pub(in crate::execution_topology_metrics) complete: bool,
    pub(in crate::execution_topology_metrics) source_state: CoverageStateV1,
}

/// A retained rollup cannot safely represent a population that exceeds its
/// explicitly bounded correction or interval carry. Callers must retain raw
/// detail/rebuild the exact day instead of persisting an approximation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::execution_topology_metrics) enum ExecutionTopologyRollupStateErrorV1 {
    #[error("execution topology rollup correction carry exceeds its bounded capacity")]
    CarryBudgetExceeded,
    #[error("execution topology rollup interval carry exceeds its bounded capacity")]
    IntervalBudgetExceeded,
    #[error("execution topology rollup state is incompatible")]
    IncompatibleState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TopologySampleV1 {
    pub(super) widths: [u16; 5],
    pub(super) interval_micros: Option<u64>,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ConflictPredictionRowV1 {
    pub(super) kind: ConflictKindV1,
    pub(super) prediction: ConflictPredictionV1,
    pub(super) coverage: CoverageStateV1,
    /// Exact envelope time anchors bounded correction-carry expiry.
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ConflictOutcomeRowV1 {
    pub(super) kind: ConflictKindV1,
    pub(super) outcome: ConflictOutcomeV1,
    pub(super) coverage: CoverageStateV1,
    /// Late correction revision. Only the highest revision for a prediction
    /// reference is evidence, so a corrected outcome never double counts.
    pub(super) correction_revision: u32,
    /// Exact envelope time anchors bounded correction-carry expiry.
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct IntegrationRowV1 {
    pub(super) phase: IntegrationPhaseV1,
    pub(super) result: IntegrationResultV1,
    pub(super) operation: IntegrationOperationKindV1,
    pub(super) coverage: CoverageStateV1,
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct StackDriftRowV1 {
    pub(super) kind: StackDriftKindV1,
    pub(super) state: IntervalStateV1,
    pub(super) first_observed_micros: i64,
    pub(super) terminal_micros: Option<i64>,
    pub(super) age_bucket: DurationBucketV1,
    pub(super) coverage: CoverageStateV1,
    pub(super) event_time_micros: i64,
    pub(super) observation_time_micros: i64,
    pub(super) producer_sequence: u64,
    pub(super) content_digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct DuplicateRowV1 {
    pub(super) kind: DuplicateEffortKindV1,
    pub(super) quantities: [Option<u64>; 5],
    pub(super) effect_outcome: DuplicateEffectOutcomeV1,
    pub(super) coverage: CoverageStateV1,
    /// Exact envelope time anchors bounded correction-carry expiry.
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct BlockedRowV1 {
    /// Stable owner receipt identity. Open and closed corrections share this
    /// trace while distinct pauses remain distinct even when cause and start
    /// time happen to coincide.
    pub(super) receipt_ref: String,
    pub(super) cause: BlockedCauseV1,
    pub(super) revision: u32,
    pub(super) valid_from_micros: i64,
    pub(super) valid_until_micros: Option<i64>,
    pub(super) coverage: CoverageStateV1,
    /// Exact envelope time anchors bounded revision-carry expiry.
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RerunRowV1 {
    pub(super) source: RerunSourceV1,
    pub(super) cause: RerunCauseV1,
    pub(super) eligible: u64,
    pub(super) linked: u64,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct LeakRowV1 {
    pub(super) kind: WorkExecutionLeakKindV1,
    pub(super) recovery: WorkExecutionLeakRecoveryV1,
    pub(super) coverage: CoverageStateV1,
    /// Exact envelope time anchors bounded correction-carry expiry.
    pub(super) event_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct FanoutRowV1 {
    pub(super) surface: DeliverySurfaceFamilyV1,
    pub(super) attempted: u64,
    pub(super) delivered: u64,
    pub(super) deduplicated: u64,
    pub(super) dropped: u64,
    pub(super) unknown: u64,
    pub(super) coverage: CoverageStateV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct GitHubStackCapabilityRowV1 {
    pub(super) capability: GitHubStackCapabilityV1,
    pub(super) standard_git_fallback_available: bool,
    pub(super) other_forge_fallback_available: bool,
    pub(super) coverage: CoverageStateV1,
    pub(super) event_time_micros: i64,
    pub(super) observation_time_micros: i64,
    pub(super) producer_sequence: u64,
    /// Content-only deterministic tie breaker; source event identity is never
    /// retained in an aggregate.
    pub(super) content_digest: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ExecutionTopologyEvidenceV1 {
    pub(super) topology: Vec<TopologySampleV1>,
    pub(super) predictions: BTreeMap<String, ConflictPredictionRowV1>,
    pub(super) outcomes: BTreeMap<String, ConflictOutcomeRowV1>,
    pub(super) integrations: Vec<IntegrationRowV1>,
    pub(super) stack_drifts: BTreeMap<String, StackDriftRowV1>,
    pub(super) duplicates: BTreeMap<String, (Option<DuplicateRowV1>, i64)>,
    pub(super) blocked: Vec<BlockedRowV1>,
    pub(super) reruns: Vec<RerunRowV1>,
    pub(super) leaks: BTreeMap<String, (u64, Option<LeakRowV1>, i64)>,
    pub(super) fanout: Vec<FanoutRowV1>,
    pub(super) github_stack_capability: Option<GitHubStackCapabilityRowV1>,
    pub(super) invalid_events: u64,
    invalid_correction_keys: BTreeSet<(u8, String, u64)>,
}

impl ExecutionTopologyEvidenceV1 {
    pub(super) fn absorb(&mut self, envelope: &ObservabilityEnvelopeV1) {
        let trace_id = envelope.trace_id.as_str();
        let event_time_micros = envelope.event_time_micros;
        let observation_time_micros = envelope.observation_time_micros;
        let valid_from_micros = envelope.valid_from_micros;
        let valid_until_micros = envelope.valid_until_micros;
        match &envelope.payload {
            ObservabilityPayloadV1::ExecutionTopology(sample) => {
                self.topology.push(TopologySampleV1 {
                    widths: [
                        sample.requested_width,
                        sample.accepted_width,
                        sample.admitted_width,
                        sample.active_width,
                        sample.useful_width,
                    ],
                    interval_micros: bounded_interval(valid_from_micros, valid_until_micros),
                    // A sample's own coverage travels on the envelope: a
                    // sample read under anything but `Known` cannot anchor a
                    // duration-weighted denominator.
                    coverage: envelope.coverage,
                });
            }
            ObservabilityPayloadV1::WorkConflictPrediction(prediction) => {
                let row = ConflictPredictionRowV1 {
                    kind: prediction.kind,
                    prediction: prediction.prediction,
                    coverage: prediction.coverage,
                    event_time_micros,
                };
                match self.predictions.get(&prediction.prediction_ref) {
                    Some(existing) if existing != &row => {
                        if self.invalid_correction_keys.insert((
                            0,
                            prediction.prediction_ref.clone(),
                            0,
                        )) {
                            self.invalid_events = self.invalid_events.saturating_add(1);
                        }
                    }
                    Some(_) => {}
                    None => {
                        self.predictions
                            .insert(prediction.prediction_ref.clone(), row);
                    }
                }
            }
            ObservabilityPayloadV1::WorkConflictOutcome(outcome) => {
                let row = ConflictOutcomeRowV1 {
                    kind: outcome.kind,
                    outcome: outcome.outcome,
                    coverage: outcome.coverage,
                    correction_revision: outcome.correction_revision,
                    event_time_micros,
                };
                match self.outcomes.get(&outcome.prediction_ref) {
                    Some(existing) if existing.correction_revision > row.correction_revision => {}
                    Some(existing) if existing.correction_revision == row.correction_revision => {
                        if existing != &row {
                            if self.invalid_correction_keys.insert((
                                1,
                                outcome.prediction_ref.clone(),
                                u64::from(row.correction_revision),
                            )) {
                                self.invalid_events = self.invalid_events.saturating_add(1);
                            }
                        }
                    }
                    _ => {
                        self.outcomes.insert(outcome.prediction_ref.clone(), row);
                    }
                }
            }
            ObservabilityPayloadV1::WorkIntegrationTransition(transition) => {
                let row = IntegrationRowV1 {
                    phase: transition.phase,
                    result: transition.result,
                    operation: transition.operation,
                    coverage: transition.coverage,
                    event_time_micros,
                };
                self.integrations.push(row);
            }
            ObservabilityPayloadV1::WorkStackDrift(drift) => {
                let content_digest = match canonical_sha256(&(
                    drift.kind,
                    drift.state,
                    drift.first_observed_micros,
                    drift.terminal_micros,
                    drift.age_bucket,
                    drift.coverage,
                )) {
                    Ok(digest) => digest.as_str().to_owned(),
                    Err(_) => {
                        self.invalid_events = self.invalid_events.saturating_add(1);
                        return;
                    }
                };
                let row = StackDriftRowV1 {
                    kind: drift.kind,
                    state: drift.state,
                    first_observed_micros: drift.first_observed_micros,
                    terminal_micros: drift.terminal_micros,
                    age_bucket: drift.age_bucket,
                    coverage: drift.coverage,
                    event_time_micros,
                    observation_time_micros,
                    producer_sequence: envelope.producer_sequence,
                    content_digest,
                };
                match self.stack_drifts.get(trace_id) {
                    Some(current) if !same_stack_drift_interval(&row, current) => {
                        if self
                            .invalid_correction_keys
                            .insert((2, trace_id.to_owned(), 0))
                        {
                            self.invalid_events = self.invalid_events.saturating_add(1);
                        }
                    }
                    Some(current) if !stack_drift_later(&row, current) => {}
                    _ => {
                        self.stack_drifts.insert(trace_id.to_owned(), row);
                    }
                }
            }
            ObservabilityPayloadV1::GitHubStackCapability(capability) => {
                let content_digest = match canonical_sha256(&(
                    capability.capability,
                    capability.standard_git_fallback_available,
                    capability.other_forge_fallback_available,
                    capability.coverage,
                )) {
                    Ok(digest) => digest.as_str().to_owned(),
                    Err(_) => {
                        self.invalid_events = self.invalid_events.saturating_add(1);
                        return;
                    }
                };
                let is_later = self.github_stack_capability.as_ref().is_none_or(|current| {
                    (
                        event_time_micros,
                        observation_time_micros,
                        envelope.producer_sequence,
                        content_digest.as_str(),
                    ) > (
                        current.event_time_micros,
                        current.observation_time_micros,
                        current.producer_sequence,
                        current.content_digest.as_str(),
                    )
                });
                if is_later {
                    self.github_stack_capability = Some(GitHubStackCapabilityRowV1 {
                        capability: capability.capability,
                        standard_git_fallback_available: capability.standard_git_fallback_available,
                        other_forge_fallback_available: capability.other_forge_fallback_available,
                        coverage: capability.coverage,
                        event_time_micros,
                        observation_time_micros,
                        producer_sequence: envelope.producer_sequence,
                        content_digest,
                    });
                }
            }
            ObservabilityPayloadV1::WorkDuplicateEffort(duplicate) => {
                self.absorb_duplicate(duplicate, event_time_micros);
            }
            ObservabilityPayloadV1::WorkBlockedInterval(interval) => {
                self.blocked.push(BlockedRowV1 {
                    receipt_ref: trace_id.to_owned(),
                    cause: interval.cause,
                    revision: interval.interval_revision,
                    valid_from_micros: interval.valid_from_micros,
                    valid_until_micros: interval.valid_until_micros,
                    coverage: interval.coverage,
                    event_time_micros,
                });
            }
            ObservabilityPayloadV1::WorkRerun(rerun) => {
                self.reruns.push(RerunRowV1 {
                    source: rerun.source,
                    cause: rerun.cause,
                    eligible: u64::from(rerun.eligible_original_count),
                    linked: u64::from(rerun.linked_rerun_count),
                    coverage: rerun.coverage,
                });
            }
            ObservabilityPayloadV1::WorkExecutionLeak(leak) => {
                self.absorb_leak(leak, event_time_micros);
            }
            ObservabilityPayloadV1::WorkDeliveryFanout(fanout) => {
                self.fanout.push(FanoutRowV1 {
                    surface: fanout.surface,
                    attempted: u64::from(fanout.attempted),
                    delivered: u64::from(fanout.delivered),
                    deduplicated: u64::from(fanout.deduplicated),
                    dropped: u64::from(fanout.dropped),
                    unknown: u64::from(fanout.unknown),
                    coverage: envelope.coverage,
                });
            }
            _ => {}
        }
    }

    fn absorb_duplicate(
        &mut self,
        duplicate: &tracedecay_domain::WorkDuplicateEffortObservedV1,
        event_time_micros: i64,
    ) {
        let row = DuplicateRowV1 {
            kind: duplicate.kind,
            quantities: [
                duplicate.wall_micros,
                duplicate.token_count,
                duplicate.cost_micros,
                duplicate.test_count,
                duplicate.effect_count,
            ],
            effect_outcome: duplicate.effect_outcome,
            coverage: duplicate.coverage,
            event_time_micros,
        };
        let Some(anchor_ref) = duplicate.local_anchor_refs.iter().min() else {
            return;
        };
        match self.duplicates.get(anchor_ref) {
            Some((existing, existing_time)) if existing.as_ref() != Some(&row) => {
                self.duplicates.insert(
                    anchor_ref.clone(),
                    (None, (*existing_time).max(event_time_micros)),
                );
            }
            Some(_) => {}
            None => {
                self.duplicates
                    .insert(anchor_ref.clone(), (Some(row), event_time_micros));
            }
        }
    }

    fn absorb_leak(
        &mut self,
        leak: &tracedecay_domain::WorkExecutionLeakObservedV1,
        event_time_micros: i64,
    ) {
        let row = LeakRowV1 {
            kind: leak.kind,
            recovery: leak.recovery,
            coverage: leak.coverage,
            event_time_micros,
        };
        match self.leaks.get(&leak.adjudication_ref) {
            Some((revision, _, _)) if *revision > leak.adjudication_revision => {}
            Some((revision, existing, existing_time))
                if *revision == leak.adjudication_revision =>
            {
                if existing.is_some_and(|existing| existing != row) {
                    self.leaks.insert(
                        leak.adjudication_ref.clone(),
                        (
                            leak.adjudication_revision,
                            None,
                            (*existing_time).max(event_time_micros),
                        ),
                    );
                }
            }
            _ => {
                self.leaks.insert(
                    leak.adjudication_ref.clone(),
                    (leak.adjudication_revision, Some(row), event_time_micros),
                );
            }
        }
    }
}

pub(super) fn stack_drift_later(incoming: &StackDriftRowV1, current: &StackDriftRowV1) -> bool {
    match (current.state, incoming.state) {
        (IntervalStateV1::Closed, IntervalStateV1::Open) => return false,
        (IntervalStateV1::Open, IntervalStateV1::Closed) => return true,
        _ => {}
    }
    (
        incoming.event_time_micros,
        incoming.observation_time_micros,
        incoming.producer_sequence,
        incoming.content_digest.as_str(),
    ) > (
        current.event_time_micros,
        current.observation_time_micros,
        current.producer_sequence,
        current.content_digest.as_str(),
    )
}

pub(super) fn same_stack_drift_interval(
    incoming: &StackDriftRowV1,
    current: &StackDriftRowV1,
) -> bool {
    incoming.kind == current.kind && incoming.first_observed_micros == current.first_observed_micros
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod aggregation_tests {
    use super::*;
    use tracedecay_domain::{
        DuplicateEffectOutcomeV1, DuplicateEffortKindV1, QuantityEvidenceClassV1,
        WorkDuplicateEffortObservedV1, WorkExecutionLeakKindV1, WorkExecutionLeakObservedV1,
        WorkExecutionLeakRecoveryV1,
    };

    fn duplicate(anchor_refs: &[&str], wall_micros: u64) -> WorkDuplicateEffortObservedV1 {
        WorkDuplicateEffortObservedV1 {
            adjudication_ref: "duplicate.relation.fixture".to_owned(),
            adjudication_revision: 1,
            kind: DuplicateEffortKindV1::ExactDuplicate,
            wall_micros: Some(wall_micros),
            token_count: None,
            cost_micros: None,
            test_count: None,
            effect_count: None,
            evidence: QuantityEvidenceClassV1::OwnerReceipt,
            effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: anchor_refs
                .iter()
                .map(|anchor| (*anchor).to_owned())
                .collect(),
        }
    }

    #[test]
    fn duplicate_effort_aggregation_is_anchor_keyed_and_order_independent() {
        for rows in [
            vec![
                duplicate(&["receipt.duplicate.alpha"], 10),
                duplicate(&["receipt.duplicate.beta"], 20),
            ],
            vec![
                duplicate(&["receipt.duplicate.beta"], 20),
                duplicate(&["receipt.duplicate.alpha"], 10),
            ],
            vec![
                duplicate(&["receipt.duplicate.alpha"], 10),
                duplicate(&["receipt.duplicate.alpha"], 10),
                duplicate(&["receipt.duplicate.beta"], 20),
            ],
        ] {
            let mut evidence = ExecutionTopologyEvidenceV1::default();
            for row in &rows {
                evidence.absorb_duplicate(row, 0);
            }
            let (row, _) = evidence.duplicates.get("receipt.duplicate.alpha").unwrap();
            assert_eq!(row.unwrap().quantities[0], Some(10));
            let (row, _) = evidence.duplicates.get("receipt.duplicate.beta").unwrap();
            assert_eq!(row.unwrap().quantities[0], Some(20));
        }
    }

    #[test]
    fn conflicting_duplicate_quantities_remain_unknown() {
        let mut evidence = ExecutionTopologyEvidenceV1::default();
        evidence.absorb_duplicate(&duplicate(&["receipt.duplicate.alpha"], 20), 0);
        evidence.absorb_duplicate(&duplicate(&["receipt.duplicate.alpha"], 21), 0);
        assert!(
            evidence.duplicates["receipt.duplicate.alpha"].0.is_none(),
            "conflicting anchor quantities must not pick an arrival-order winner"
        );
    }

    fn leak(revision: u64, recovery: WorkExecutionLeakRecoveryV1) -> WorkExecutionLeakObservedV1 {
        WorkExecutionLeakObservedV1 {
            adjudication_ref: "leak-adjudication:fixture".to_owned(),
            adjudication_revision: revision,
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            detection_horizon_micros: 60_000_000,
            recovery,
            owner_class: tracedecay_domain::LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
        }
    }

    #[test]
    fn leak_adjudication_is_revision_monotone_and_order_independent() {
        for rows in [
            vec![
                leak(1, WorkExecutionLeakRecoveryV1::Pending),
                leak(2, WorkExecutionLeakRecoveryV1::Recovered),
            ],
            vec![
                leak(2, WorkExecutionLeakRecoveryV1::Recovered),
                leak(1, WorkExecutionLeakRecoveryV1::Pending),
            ],
        ] {
            let mut evidence = ExecutionTopologyEvidenceV1::default();
            for row in &rows {
                evidence.absorb_leak(row, 0);
            }
            let (_, row, _) = evidence.leaks.get("leak-adjudication:fixture").unwrap();
            assert_eq!(
                row.unwrap().recovery,
                WorkExecutionLeakRecoveryV1::Recovered
            );
        }
    }

    #[test]
    fn conflicting_latest_leak_revision_remains_unknown() {
        let mut evidence = ExecutionTopologyEvidenceV1::default();
        evidence.absorb_leak(&leak(2, WorkExecutionLeakRecoveryV1::Recovered), 0);
        evidence.absorb_leak(&leak(2, WorkExecutionLeakRecoveryV1::Failed), 0);
        assert!(evidence.leaks["leak-adjudication:fixture"].1.is_none());
    }
}
