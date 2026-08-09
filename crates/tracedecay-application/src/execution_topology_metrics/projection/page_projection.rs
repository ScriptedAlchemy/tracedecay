//! Classification and finalization for one bounded execution-topology page.
//!
//! Keeping ephemeral classification separate from family formulas lets daily
//! rollups reduce events into bounded sufficient statistics without retaining
//! raw envelopes or event-scale classified rows.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CoverageStateV1, ObservabilityPayloadV1, canonical_sha256};

use crate::observability::{MetricCoverageV1, ObservabilityHorizonV1, ObservabilityPageV1};

use super::capacity_corrections::ExecutionTopologyCapacityCorrectionCarryV1;
use super::capacity_rollup::ExecutionTopologyCapacityRollupV1;
use super::lifecycle_rollup::{
    ExecutionTopologyLifecycleCarryV1, ExecutionTopologyLifecycleRollupV1,
};
use super::{
    ExecutionTopologyEvidenceV1, ExecutionTopologyRollupStateErrorV1, ProjectionContext,
    TELEMETRY_DROP_EVENT_KIND_V1,
};
use crate::execution_topology_metrics::support::{unavailable_model_at, worse_state};
use crate::execution_topology_metrics::{
    EXECUTION_TOPOLOGY_EVENT_KINDS_V1, ExecutionGitHubStackCapabilityReadingV1,
    ExecutionMetricUnavailableV1, ExecutionTopologyDrillAnchorV1,
    ExecutionTopologyEmissionCoverageV1, ExecutionTopologyMetricsV1,
    MAX_EXECUTION_TOPOLOGY_CELLS_V1, MAX_EXECUTION_TOPOLOGY_DRILL_ANCHORS_V1,
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1, MIN_EXECUTION_TOPOLOGY_LOCAL_CELL_SUPPORT_V1,
};

/// Opaque join key for reconciling an explicit producer-loss receipt with its
/// next admitted envelope. It is retained only while composing local rollups;
/// it never becomes a metric dimension or read-model field.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(super) struct DropCarrierJoinV1 {
    pub(super) process_boot_ref: String,
    pub(super) sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ExplicitDropReceiptV1 {
    pub(super) join: DropCarrierJoinV1,
    /// The receipt event's observed time bounds how long this unresolved
    /// producer-loss join may remain in retained correction carry.
    pub(super) event_time_micros: i64,
    /// `None` records conflicting receipts for the same join key. We retain
    /// the conflict instead of choosing a loss count.
    pub(super) proved_drop_lower_bound: Option<u64>,
    pub(super) first_missing_sequence: Option<u64>,
    pub(super) clean_shutdown_observed: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct DropCarrierV1 {
    pub(super) join: DropCarrierJoinV1,
    pub(super) dropped_count: u64,
    /// The carrying topology envelope bounds its retained join lifetime.
    pub(super) event_time_micros: i64,
}

/// Classified, bounded evidence from one exact authorized page. Unlike the
/// page, this excludes envelopes and payloads: it retains only metric classes,
/// opaque correction joins, producer-loss joins, and bounded drill cursors.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::execution_topology_metrics) struct ClassifiedExecutionTopologyPageV1 {
    pub(super) evidence: ExecutionTopologyEvidenceV1,
    pub(super) emitted: u64,
    pub(super) delayed: u64,
    pub(super) sampled_events: u64,
    pub(super) replayed: u64,
    pub(super) payload_coverage_state: CoverageStateV1,
    pub(super) source_coverage_state: CoverageStateV1,
    pub(super) explicit_drop_receipts: Vec<ExplicitDropReceiptV1>,
    pub(super) drop_carriers: Vec<DropCarrierV1>,
    pub(super) drill_cursors: Vec<String>,
    pub(super) watermark: String,
}

impl ClassifiedExecutionTopologyPageV1 {
    pub(in crate::execution_topology_metrics) fn watermark(&self) -> &str {
        &self.watermark
    }

    pub(in crate::execution_topology_metrics) fn source_is_stale(&self) -> bool {
        self.source_coverage_state == CoverageStateV1::Stale
    }

    pub(in crate::execution_topology_metrics) fn drill_cursors(&self) -> &[String] {
        &self.drill_cursors
    }

    pub(in crate::execution_topology_metrics) fn is_valid_rollup_state(&self) -> bool {
        self.delayed <= self.emitted
            && self.drill_cursors.len() <= MAX_EXECUTION_TOPOLOGY_DRILL_ANCHORS_V1
            && self.drill_cursors.iter().all(|cursor| safe_cursor(cursor))
    }
}

/// One canonical projection from retained sufficient statistics.
#[derive(Clone, Debug)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyRollupProjectionV1 {
    pub(in crate::execution_topology_metrics) model: ExecutionTopologyMetricsV1,
}

const PRODUCER_DETAIL_RETENTION_MICROS_V1: i64 = 30 * 86_400_000_000;
const MAX_EXECUTION_TOPOLOGY_PRODUCER_DROP_CARRY_V1: usize = 512;

/// The only persisted state that crosses metric families. Its aggregate
/// members contain fixed-size sufficient statistics; each carry is bounded
/// and contains just the still-unresolved correction edge it must reconcile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyReducedRollupStateV1 {
    capacity: ExecutionTopologyCapacityRollupV1,
    capacity_carry: ExecutionTopologyCapacityCorrectionCarryV1,
    lifecycle: ExecutionTopologyLifecycleRollupV1,
    lifecycle_carry: ExecutionTopologyLifecycleCarryV1,
    producer: ExecutionTopologyProducerRollupV1,
    /// The latest retention frontier evaluated for this bounded state. Opaque
    /// unresolved joins remain until they can be settled exactly.
    retention_checked_before_micros: Option<i64>,
}

/// Aggregate producer coverage and the opaque loss edges that are still able
/// to join a neighboring UTC day. Process and sequence identifiers are hashed
/// before this state is serialized.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutionTopologyProducerRollupV1 {
    emitted: u64,
    delayed: u64,
    sampled_events: u64,
    replayed: u64,
    invalid_events: u64,
    payload_coverage_state: CoverageStateV1,
    source_coverage_state: CoverageStateV1,
    dropped: u64,
    drop_carry: BTreeMap<String, ExecutionTopologyProducerDropCarryV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecutionTopologyProducerDropCarryV1 {
    receipt_seen: bool,
    receipt_lower_bound: Option<u64>,
    carrier_dropped_count: Option<u64>,
    event_time_micros: i64,
}

impl ExecutionTopologyReducedRollupStateV1 {
    pub(in crate::execution_topology_metrics) fn validate(
        &self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.capacity.validate()?;
        self.capacity_carry.validate()?;
        self.lifecycle.validate()?;
        self.lifecycle_carry.validate()?;
        self.producer.validate()
    }

    pub(in crate::execution_topology_metrics) fn validate_for_horizon(
        &self,
        horizon: &ObservabilityHorizonV1,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.validate()?;
        self.lifecycle.validate_for_horizon(horizon)?;
        if !self
            .capacity_carry
            .event_times_within(horizon.since_micros, horizon.until_micros)
            || !self
                .lifecycle_carry
                .event_times_within(horizon.since_micros, horizon.until_micros)
            || !self
                .producer
                .event_times_within(horizon.since_micros, horizon.until_micros)
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn source_is_stale(&self) -> bool {
        self.producer.source_coverage_state == CoverageStateV1::Stale
    }

    pub(in crate::execution_topology_metrics) fn merge(
        &mut self,
        other: Self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.capacity.merge(other.capacity)?;
        let newly_invalid = self.capacity_carry.merge(other.capacity_carry)?;
        self.lifecycle.merge(other.lifecycle)?;
        self.lifecycle_carry.merge(other.lifecycle_carry)?;
        self.producer.merge(other.producer)?;
        self.producer.invalid_events = self.producer.invalid_events.saturating_add(newly_invalid);
        self.retention_checked_before_micros = match (
            self.retention_checked_before_micros,
            other.retention_checked_before_micros,
        ) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(cutoff), None) | (None, Some(cutoff)) => Some(cutoff),
            (None, None) => None,
        };
        self.validate()
    }

    pub(in crate::execution_topology_metrics) fn check_retention(
        &mut self,
        now_micros: i64,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        let cutoff = now_micros
            .checked_sub(PRODUCER_DETAIL_RETENTION_MICROS_V1)
            .ok_or(ExecutionTopologyRollupStateErrorV1::IncompatibleState)?;
        self.retention_checked_before_micros = Some(
            self.retention_checked_before_micros
                .map_or(cutoff, |existing| existing.max(cutoff)),
        );
        self.validate()
    }
}

impl ExecutionTopologyProducerRollupV1 {
    fn validate(&self) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.check_carry()?;
        if self.delayed > self.emitted
            || self.drop_carry.iter().any(|(key, edge)| {
                !protected_rollup_key_is_valid(key)
                    || (!edge.receipt_seen && edge.receipt_lower_bound.is_some())
                    || (!edge.receipt_seen && edge.carrier_dropped_count.is_none())
                    || (edge.receipt_seen && edge.carrier_dropped_count.is_some())
                    || edge.carrier_dropped_count == Some(0)
            })
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    fn event_times_within(&self, since_micros: i64, until_micros: i64) -> bool {
        self.drop_carry.values().all(|edge| {
            edge.event_time_micros >= since_micros && edge.event_time_micros < until_micros
        })
    }

    fn from_classified(
        classified: &ClassifiedExecutionTopologyPageV1,
    ) -> Result<Self, ExecutionTopologyRollupStateErrorV1> {
        let mut producer = Self {
            emitted: classified.emitted,
            delayed: classified.delayed,
            sampled_events: classified.sampled_events,
            replayed: classified.replayed,
            invalid_events: classified.evidence.invalid_events,
            payload_coverage_state: classified.payload_coverage_state,
            source_coverage_state: classified.source_coverage_state,
            dropped: 0,
            drop_carry: BTreeMap::new(),
        };
        for receipt in &classified.explicit_drop_receipts {
            let key = producer_drop_key(&receipt.join)?;
            producer.absorb_receipt(
                key,
                receipt.proved_drop_lower_bound,
                receipt.event_time_micros,
            );
        }
        for carrier in &classified.drop_carriers {
            let key = producer_drop_key(&carrier.join)?;
            producer.absorb_carrier(key, carrier.dropped_count, carrier.event_time_micros);
        }
        producer.fold_resolved_edges();
        producer.check_carry()?;
        Ok(producer)
    }

    fn merge(&mut self, other: Self) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.emitted = self.emitted.saturating_add(other.emitted);
        self.delayed = self.delayed.saturating_add(other.delayed);
        self.sampled_events = self.sampled_events.saturating_add(other.sampled_events);
        self.replayed = self.replayed.saturating_add(other.replayed);
        self.invalid_events = self.invalid_events.saturating_add(other.invalid_events);
        self.payload_coverage_state =
            worse_state(self.payload_coverage_state, other.payload_coverage_state);
        self.source_coverage_state =
            worse_state(self.source_coverage_state, other.source_coverage_state);
        self.dropped = self.dropped.saturating_add(other.dropped);
        for (key, incoming) in other.drop_carry {
            match self.drop_carry.get_mut(&key) {
                None => {
                    self.drop_carry.insert(key, incoming);
                }
                Some(existing) => merge_drop_edge(existing, incoming, &mut self.invalid_events),
            }
        }
        self.fold_resolved_edges();
        self.check_carry()
    }

    fn total_dropped(&self) -> u64 {
        self.drop_carry.values().fold(self.dropped, |total, edge| {
            total.saturating_add(edge.resolved_dropped())
        })
    }

    fn absorb_receipt(&mut self, key: String, bound: Option<u64>, event_time_micros: i64) {
        let edge =
            self.drop_carry
                .entry(key)
                .or_insert_with(|| ExecutionTopologyProducerDropCarryV1 {
                    receipt_seen: false,
                    receipt_lower_bound: None,
                    carrier_dropped_count: None,
                    event_time_micros,
                });
        if edge.receipt_seen && edge.receipt_lower_bound != bound {
            edge.receipt_lower_bound = None;
            self.invalid_events = self.invalid_events.saturating_add(1);
        } else {
            edge.receipt_seen = true;
            edge.receipt_lower_bound = bound;
        }
        edge.event_time_micros = edge.event_time_micros.max(event_time_micros);
    }

    fn absorb_carrier(&mut self, key: String, dropped_count: u64, event_time_micros: i64) {
        let edge =
            self.drop_carry
                .entry(key)
                .or_insert_with(|| ExecutionTopologyProducerDropCarryV1 {
                    receipt_seen: false,
                    receipt_lower_bound: None,
                    carrier_dropped_count: None,
                    event_time_micros,
                });
        if edge
            .carrier_dropped_count
            .is_some_and(|existing| existing != dropped_count)
        {
            self.invalid_events = self.invalid_events.saturating_add(1);
            edge.carrier_dropped_count = None;
        } else {
            edge.carrier_dropped_count = Some(dropped_count);
        }
        edge.event_time_micros = edge.event_time_micros.max(event_time_micros);
    }

    fn fold_resolved_edges(&mut self) {
        let mut unresolved = BTreeMap::new();
        for (key, edge) in std::mem::take(&mut self.drop_carry) {
            if edge.receipt_seen && edge.carrier_dropped_count.is_some() {
                self.dropped = self.dropped.saturating_add(edge.resolved_dropped());
            } else {
                unresolved.insert(key, edge);
            }
        }
        self.drop_carry = unresolved;
    }

    fn check_carry(&self) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        if self.drop_carry.len() > MAX_EXECUTION_TOPOLOGY_PRODUCER_DROP_CARRY_V1 {
            return Err(ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded);
        }
        Ok(())
    }
}

impl ExecutionTopologyProducerDropCarryV1 {
    fn resolved_dropped(&self) -> u64 {
        match (self.receipt_lower_bound, self.carrier_dropped_count) {
            (Some(receipt), Some(carrier)) => receipt.max(carrier),
            (Some(receipt), None) => receipt,
            (None, Some(carrier)) => carrier,
            (None, None) => 0,
        }
    }
}

fn merge_drop_edge(
    target: &mut ExecutionTopologyProducerDropCarryV1,
    incoming: ExecutionTopologyProducerDropCarryV1,
    invalid_events: &mut u64,
) {
    if target.receipt_seen
        && incoming.receipt_seen
        && target.receipt_lower_bound != incoming.receipt_lower_bound
    {
        target.receipt_lower_bound = None;
        *invalid_events = invalid_events.saturating_add(1);
    } else if incoming.receipt_seen {
        target.receipt_seen = true;
        target.receipt_lower_bound = incoming.receipt_lower_bound;
    }
    if let (Some(left), Some(right)) =
        (target.carrier_dropped_count, incoming.carrier_dropped_count)
    {
        if left != right {
            target.carrier_dropped_count = None;
            *invalid_events = invalid_events.saturating_add(1);
        }
    } else if incoming.carrier_dropped_count.is_some() {
        target.carrier_dropped_count = incoming.carrier_dropped_count;
    }
    target.event_time_micros = target.event_time_micros.max(incoming.event_time_micros);
}

fn producer_drop_key(
    join: &DropCarrierJoinV1,
) -> Result<String, ExecutionTopologyRollupStateErrorV1> {
    canonical_sha256(&("execution-topology.producer-drop", join))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)
}

pub(in crate::execution_topology_metrics) fn reduce_classified_execution_topology_rollup_state(
    horizon: &ObservabilityHorizonV1,
    classified: &ClassifiedExecutionTopologyPageV1,
) -> Result<ExecutionTopologyReducedRollupStateV1, ExecutionTopologyRollupStateErrorV1> {
    let capacity = classified.evidence.reduce_capacity_rollup()?;
    let capacity_carry = classified.evidence.reduce_capacity_correction_carry()?;
    let (lifecycle, lifecycle_carry) = classified.evidence.reduce_lifecycle_rollup(horizon)?;
    let producer = ExecutionTopologyProducerRollupV1::from_classified(classified)?;
    let reduced = ExecutionTopologyReducedRollupStateV1 {
        capacity,
        capacity_carry,
        lifecycle,
        lifecycle_carry,
        producer,
        retention_checked_before_micros: None,
    };
    reduced.validate()?;
    Ok(reduced)
}

/// Classifies one fully read page into the bounded evidence that the projector
/// actually consumes. Rollups serialize this result, never the input page.
pub(in crate::execution_topology_metrics) fn classify_execution_topology_page(
    authorized_scope_ref: &str,
    horizon: &ObservabilityHorizonV1,
    page: ObservabilityPageV1,
) -> Result<ClassifiedExecutionTopologyPageV1, ExecutionMetricUnavailableV1> {
    if page.next_watermark.is_some()
        || page.events.len() as u64 > u64::from(MAX_EXECUTION_TOPOLOGY_EVENTS_V1)
    {
        return Err(ExecutionMetricUnavailableV1::EventBudgetExceeded);
    }
    if !safe_cursor(&page.watermark)
        || page.event_cursors.len() != page.events.len()
        || page.event_cursors.iter().any(|cursor| !safe_cursor(cursor))
        || page.event_cursors.iter().collect::<BTreeSet<_>>().len() != page.event_cursors.len()
    {
        return Err(ExecutionMetricUnavailableV1::StoreUnavailable);
    }

    let mut evidence = ExecutionTopologyEvidenceV1::default();
    let mut replayed = 0u64;
    let mut idempotency_events = BTreeMap::new();
    let mut accepted = Vec::new();
    for (index, envelope) in page.events.iter().enumerate() {
        let topology_event =
            EXECUTION_TOPOLOGY_EVENT_KINDS_V1.contains(&envelope.event_kind.as_str());
        let telemetry_drop = envelope.event_kind == TELEMETRY_DROP_EVENT_KIND_V1;
        if envelope.validate().is_err()
            || envelope.scope_ref != authorized_scope_ref
            || envelope.event_time_micros < horizon.since_micros
            || envelope.event_time_micros >= horizon.until_micros
            || (!topology_event && !telemetry_drop)
        {
            evidence.invalid_events = evidence.invalid_events.saturating_add(1);
            continue;
        }
        if let Some(existing) = idempotency_events.get(&envelope.idempotency_key) {
            if *existing == envelope {
                replayed = replayed.saturating_add(1);
            } else {
                evidence.invalid_events = evidence.invalid_events.saturating_add(1);
            }
            continue;
        }
        idempotency_events.insert(envelope.idempotency_key.clone(), envelope);
        accepted.push((index, envelope));
    }

    let mut receipt_map: BTreeMap<DropCarrierJoinV1, ExplicitDropReceiptV1> = BTreeMap::new();
    for (_, envelope) in &accepted {
        let ObservabilityPayloadV1::TelemetryDrop(drop) = &envelope.payload else {
            continue;
        };
        let receipt = ExplicitDropReceiptV1 {
            join: DropCarrierJoinV1 {
                process_boot_ref: envelope.process_boot_id.clone(),
                sequence: drop.last_missing_sequence.saturating_add(1),
            },
            proved_drop_lower_bound: Some(drop.proved_drop_lower_bound),
            first_missing_sequence: Some(drop.first_missing_sequence),
            clean_shutdown_observed: Some(drop.clean_shutdown_observed),
            event_time_micros: envelope.event_time_micros,
        };
        match receipt_map.get(&receipt.join).cloned() {
            None => {
                receipt_map.insert(receipt.join.clone(), receipt);
            }
            Some(existing) if same_drop_receipt(&existing, &receipt) => {}
            Some(existing) => {
                receipt_map.insert(
                    receipt.join.clone(),
                    ExplicitDropReceiptV1 {
                        join: receipt.join,
                        proved_drop_lower_bound: None,
                        first_missing_sequence: None,
                        clean_shutdown_observed: None,
                        event_time_micros: existing
                            .event_time_micros
                            .max(receipt.event_time_micros),
                    },
                );
                evidence.invalid_events = evidence.invalid_events.saturating_add(1);
            }
        }
    }

    let mut emitted = 0u64;
    let mut delayed = 0u64;
    let mut sampled_events = 0u64;
    let mut payload_coverage_state = CoverageStateV1::Known;
    let mut drill_cursors = Vec::new();
    let mut drop_carriers = Vec::new();
    for (index, envelope) in accepted {
        if matches!(envelope.payload, ObservabilityPayloadV1::TelemetryDrop(_)) {
            continue;
        }
        let invalid_before = evidence.invalid_events;
        evidence.absorb(envelope);
        if evidence.invalid_events != invalid_before {
            continue;
        }
        emitted = emitted.saturating_add(envelope.emitted_count);
        delayed = delayed.saturating_add(envelope.delayed_count);
        if envelope.coverage == CoverageStateV1::Sampled {
            sampled_events = sampled_events.saturating_add(1);
        }
        payload_coverage_state = worse_state(payload_coverage_state, envelope.coverage);
        if let Some(payload_coverage) = execution_payload_coverage(&envelope.payload) {
            payload_coverage_state = worse_state(payload_coverage_state, payload_coverage);
        }
        if drill_cursors.len() < MAX_EXECUTION_TOPOLOGY_DRILL_ANCHORS_V1 {
            drill_cursors.push(page.event_cursors[index].clone());
        }
        if envelope.dropped_count > 0 {
            drop_carriers.push(DropCarrierV1 {
                join: DropCarrierJoinV1 {
                    process_boot_ref: envelope.process_boot_id.clone(),
                    sequence: envelope.producer_sequence,
                },
                dropped_count: envelope.dropped_count,
                event_time_micros: envelope.event_time_micros,
            });
        }
    }

    Ok(ClassifiedExecutionTopologyPageV1 {
        evidence,
        emitted,
        delayed,
        sampled_events,
        replayed,
        payload_coverage_state,
        source_coverage_state: page.coverage,
        explicit_drop_receipts: receipt_map.into_values().collect(),
        drop_carriers,
        drill_cursors,
        watermark: page.watermark,
    })
}

/// Finalizes one retained-state projection. All family formulas run from
/// aggregate sufficient statistics; no classified rows are rehydrated here.
pub(in crate::execution_topology_metrics) fn project_reduced_execution_topology_rollup_state(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    watermark: String,
    drill_anchors: Vec<ExecutionTopologyDrillAnchorV1>,
    state: &ExecutionTopologyReducedRollupStateV1,
) -> Result<ExecutionTopologyRollupProjectionV1, ExecutionTopologyRollupStateErrorV1> {
    state.validate()?;
    let dropped = state.producer.total_dropped();
    let mut source_state = state.producer.source_coverage_state;
    if state.producer.sampled_events > 0 {
        source_state = worse_state(source_state, CoverageStateV1::Sampled);
    }
    if (state.producer.delayed > 0 || dropped > 0)
        && matches!(
            source_state,
            CoverageStateV1::Known | CoverageStateV1::Sampled | CoverageStateV1::Capped
        )
    {
        source_state = CoverageStateV1::Partial;
    }
    if state.producer.invalid_events > 0 {
        source_state = worse_state(source_state, CoverageStateV1::Partial);
    }
    let complete = source_state == CoverageStateV1::Known;
    let eligible_known = state.producer.invalid_events == 0
        && !matches!(
            source_state,
            CoverageStateV1::Sampled
                | CoverageStateV1::Capped
                | CoverageStateV1::Stale
                | CoverageStateV1::Unknown
        );
    let family_coverage = MetricCoverageV1 {
        eligible: eligible_known.then_some(state.producer.emitted.saturating_add(dropped)),
        observed: state.producer.emitted,
        completed: state
            .producer
            .emitted
            .saturating_sub(state.producer.delayed),
        censored: 0,
        unknown: state.producer.invalid_events.saturating_add(dropped),
        excluded: state.producer.replayed,
        state: worse_state(source_state, state.producer.payload_coverage_state),
    };
    let projection = ProjectionContext {
        horizon: horizon.clone(),
        watermark: watermark.clone(),
        complete,
        source_state,
    };
    let capacity = state.capacity.with_carry_applied(&state.capacity_carry)?;
    let mut measurements = Vec::new();
    capacity.project(&projection, &mut measurements);
    state
        .lifecycle
        .project_with_carry(&state.lifecycle_carry, &projection, &mut measurements)?;
    let github_stack_capability = state.lifecycle.project_github_stack_capability(&projection);
    let emission_coverage = ExecutionTopologyEmissionCoverageV1 {
        emitted: Some(state.producer.emitted),
        delayed: Some(state.producer.delayed),
        dropped: Some(dropped),
        sampled_events: Some(state.producer.sampled_events),
    };
    Ok(finalize_rollup_projection(
        authorized_scope_ref,
        horizon,
        observed_at_micros,
        watermark,
        complete,
        family_coverage,
        emission_coverage,
        github_stack_capability,
        drill_anchors,
        measurements,
    ))
}

fn protected_rollup_key_is_valid(reference: &str) -> bool {
    reference.len() == 71
        && reference.starts_with("sha256:")
        && reference[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn finalize_rollup_projection(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    watermark: String,
    complete: bool,
    family_coverage: MetricCoverageV1,
    emission_coverage: ExecutionTopologyEmissionCoverageV1,
    github_stack_capability: ExecutionGitHubStackCapabilityReadingV1,
    drill_anchors: Vec<ExecutionTopologyDrillAnchorV1>,
    mut measurements: Vec<super::super::ExecutionTopologyMeasurementV1>,
) -> ExecutionTopologyRollupProjectionV1 {
    suppress_low_support_cells(&mut measurements);
    if measurements.len() > MAX_EXECUTION_TOPOLOGY_CELLS_V1 {
        let mut capped = unavailable_model_at(
            authorized_scope_ref,
            horizon,
            observed_at_micros,
            watermark,
            ExecutionMetricUnavailableV1::CellBudgetExceeded,
        );
        capped.coverage = MetricCoverageV1 {
            state: CoverageStateV1::Capped,
            ..family_coverage
        };
        capped.emission_coverage = emission_coverage;
        capped.github_stack_capability = ExecutionGitHubStackCapabilityReadingV1 {
            capability: None,
            standard_git_fallback_available: None,
            other_forge_fallback_available: None,
            coverage: MetricCoverageV1 {
                eligible: None,
                observed: 0,
                completed: 0,
                censored: 0,
                unknown: 1,
                excluded: 0,
                state: CoverageStateV1::Capped,
            },
            unavailable: Some(ExecutionMetricUnavailableV1::CellBudgetExceeded),
        };
        capped.drill_anchors = drill_anchors;
        return ExecutionTopologyRollupProjectionV1 { model: capped };
    }
    ExecutionTopologyRollupProjectionV1 {
        model: ExecutionTopologyMetricsV1 {
            authorized_scope_ref,
            horizon,
            watermark,
            observed_at_micros,
            current: complete,
            coverage: family_coverage,
            emission_coverage,
            github_stack_capability,
            drill_anchors,
            measurements,
        },
    }
}

fn suppress_low_support_cells(measurements: &mut [super::super::ExecutionTopologyMeasurementV1]) {
    for measurement in measurements {
        let support = measurement.local_support();
        if support == 0 || support >= MIN_EXECUTION_TOPOLOGY_LOCAL_CELL_SUPPORT_V1 {
            continue;
        }
        let reason = ExecutionMetricUnavailableV1::SupportFloorUnmet;
        measurement.unavailable = Some(reason);
        measurement.value.value = None;
        measurement.value.denominator_value = None;
        measurement.value.coverage = MetricCoverageV1 {
            eligible: None,
            observed: 0,
            completed: 0,
            censored: 0,
            unknown: 1,
            excluded: 0,
            state: CoverageStateV1::Unknown,
        };
        measurement.value.uncertainty.lower = None;
        measurement.value.uncertainty.upper = None;
        measurement.value.uncertainty.reason = Some(reason.as_str().to_owned());
        measurement.value.unavailable_reason = Some(reason.as_str().to_owned());
    }
}

fn same_drop_receipt(left: &ExplicitDropReceiptV1, right: &ExplicitDropReceiptV1) -> bool {
    left.join == right.join
        && left.proved_drop_lower_bound == right.proved_drop_lower_bound
        && left.first_missing_sequence == right.first_missing_sequence
        && left.clean_shutdown_observed == right.clean_shutdown_observed
}

fn safe_cursor(cursor: &str) -> bool {
    !cursor.is_empty()
        && cursor.len() <= 512
        && cursor.trim() == cursor
        && !cursor.chars().any(char::is_control)
}

fn execution_payload_coverage(payload: &ObservabilityPayloadV1) -> Option<CoverageStateV1> {
    match payload {
        ObservabilityPayloadV1::WorkConflictPrediction(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkConflictOutcome(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkIntegrationTransition(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkStackDrift(value) => Some(value.coverage),
        ObservabilityPayloadV1::GitHubStackCapability(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkDuplicateEffort(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkBlockedInterval(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkRerun(value) => Some(value.coverage),
        ObservabilityPayloadV1::WorkExecutionLeak(value) => Some(value.coverage),
        _ => None,
    }
}
