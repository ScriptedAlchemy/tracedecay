use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CoverageStateV1, DurationBucketV1, IntegrationPhaseV1, IntegrationResultV1,
    WorkStackDriftObservedV1, canonical_sha256,
};

use crate::observability::ObservabilityHorizonV1;

use super::super::{
    ExecutionBlockedCauseV1, ExecutionDurationBucketV1, ExecutionIntegrationKindV1,
    ExecutionIntegrationOutcomeV1, ExecutionIntervalStateV1, ExecutionLeakKindV1,
    ExecutionLeakOutcomeV1, ExecutionRerunCauseV1, ExecutionRerunSourceV1,
    ExecutionStackDriftKindV1, ExecutionSurfaceFamilyV1,
};
use super::{
    BlockedRowV1, ExecutionTopologyEvidenceV1, ExecutionTopologyRollupStateErrorV1,
    GitHubStackCapabilityRowV1, StackDriftRowV1, same_stack_drift_interval, stack_drift_later,
};

/// Persisted lifecycle sufficient statistics. Event-scale joins never live in
/// this value: settled families are additive, blocked time is a normalized
/// interval union, and only recent cross-day corrections stay in the carry.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyLifecycleRollupV1 {
    #[serde(with = "ordered_map_entries")]
    pub(super) merge_cells:
        BTreeMap<(ExecutionIntegrationKindV1, ExecutionIntegrationOutcomeV1), u64>,
    #[serde(with = "ordered_map_entries")]
    pub(super) merge_totals: BTreeMap<ExecutionIntegrationKindV1, (u64, u64)>,
    pub(super) merge_eligible: u64,
    pub(super) merge_unknown: u64,
    #[serde(with = "ordered_map_entries")]
    pub(super) stack_drift_cells: BTreeMap<
        (
            ExecutionStackDriftKindV1,
            ExecutionIntervalStateV1,
            ExecutionDurationBucketV1,
        ),
        u64,
    >,
    pub(super) stack_drift_eligible: u64,
    pub(super) stack_drift_unknown: u64,
    pub(super) blocked_union: Vec<(i64, i64)>,
    #[serde(with = "ordered_map_entries")]
    pub(super) blocked_cause_unions: BTreeMap<ExecutionBlockedCauseV1, Vec<(i64, i64)>>,
    #[serde(with = "ordered_map_entries", default)]
    pub(super) blocked_observed_by_cause: BTreeMap<ExecutionBlockedCauseV1, u64>,
    pub(super) blocked_eligible: u64,
    pub(super) blocked_observed: u64,
    pub(super) blocked_censored: u64,
    pub(super) blocked_unknown: u64,
    #[serde(with = "ordered_map_entries")]
    pub(super) rerun_cells: BTreeMap<(ExecutionRerunSourceV1, ExecutionRerunCauseV1), u64>,
    #[serde(with = "ordered_map_entries", default)]
    pub(super) rerun_eligible_cells: BTreeMap<(ExecutionRerunSourceV1, ExecutionRerunCauseV1), u64>,
    #[serde(with = "ordered_map_entries")]
    pub(super) rerun_totals: BTreeMap<ExecutionRerunSourceV1, (u64, u64)>,
    pub(super) rerun_eligible: u64,
    pub(super) rerun_unknown: u64,
    #[serde(with = "ordered_map_entries")]
    pub(super) leak_cells: BTreeMap<(ExecutionLeakKindV1, ExecutionLeakOutcomeV1), u64>,
    pub(super) leak_eligible: u64,
    pub(super) leak_unknown: u64,
    #[serde(with = "ordered_map_entries")]
    pub(super) delivery_totals: BTreeMap<ExecutionSurfaceFamilyV1, [u64; 5]>,
    pub(super) delivery_attempted: u64,
    pub(super) delivery_completed: u64,
    pub(super) delivery_dropped: u64,
    pub(super) delivery_unknown: u64,
    pub(super) github_stack_capability: Option<GitHubStackCapabilityRowV1>,
}

mod ordered_map_entries {
    use std::collections::BTreeMap;

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S, K, V>(map: &BTreeMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        K: Ord + Serialize,
        V: Serialize,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        D: Deserializer<'de>,
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
    {
        let entries = Vec::<(K, V)>::deserialize(deserializer)?;
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            if map.insert(key, value).is_some() {
                return Err(D::Error::custom("duplicate reduced rollup map key"));
            }
        }
        Ok(map)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::execution_topology_metrics) struct LifecycleBlockedCandidateV1 {
    pub(super) revision: u32,
    pub(super) row: Option<BlockedRowV1>,
    pub(super) created_at_micros: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::execution_topology_metrics) struct LifecycleLeakCandidateV1 {
    pub(super) revision: u64,
    pub(super) row: Option<super::LeakRowV1>,
    pub(super) created_at_micros: i64,
}

/// Recent correction and cross-day join state. Keys are domain-separated
/// SHA-256 digests, so retained rollups never persist trace or receipt text.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyLifecycleCarryV1 {
    pub(super) stack_drifts: BTreeMap<String, StackDriftRowV1>,
    pub(super) blocked: BTreeMap<String, LifecycleBlockedCandidateV1>,
    pub(super) leaks: BTreeMap<String, LifecycleLeakCandidateV1>,
}

pub(super) const MAX_EXECUTION_TOPOLOGY_LIFECYCLE_CARRY_V1: usize = 512;
pub(super) const MAX_EXECUTION_TOPOLOGY_BLOCKED_UNION_SEGMENTS_V1: usize = 512;

fn protected_key(domain: &str, value: &str) -> Result<String, ExecutionTopologyRollupStateErrorV1> {
    canonical_sha256(&(domain, value))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)
}

fn carry_len(carry: &ExecutionTopologyLifecycleCarryV1) -> usize {
    carry
        .stack_drifts
        .len()
        .saturating_add(carry.blocked.len())
        .saturating_add(carry.leaks.len())
}

fn check_carry(
    carry: &ExecutionTopologyLifecycleCarryV1,
) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
    if carry_len(carry) > MAX_EXECUTION_TOPOLOGY_LIFECYCLE_CARRY_V1 {
        Err(ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded)
    } else {
        Ok(())
    }
}

fn merge_segments(
    target: &mut Vec<(i64, i64)>,
    source: &[(i64, i64)],
) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
    target.extend(source.iter().copied().filter(|(start, end)| end >= start));
    target.sort_unstable();
    let mut merged = Vec::with_capacity(target.len());
    for (start, end) in target.drain(..) {
        if let Some((_, previous_end)) = merged.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    if merged.len() > MAX_EXECUTION_TOPOLOGY_BLOCKED_UNION_SEGMENTS_V1 {
        return Err(ExecutionTopologyRollupStateErrorV1::IntervalBudgetExceeded);
    }
    *target = merged;
    Ok(())
}

fn fold_interval(
    aggregate: &mut ExecutionTopologyLifecycleRollupV1,
    cause: ExecutionBlockedCauseV1,
    interval: (i64, i64),
) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
    merge_segments(&mut aggregate.blocked_union, &[interval])?;
    merge_segments(
        aggregate.blocked_cause_unions.entry(cause).or_default(),
        &[interval],
    )
}

impl ExecutionTopologyEvidenceV1 {
    pub(in crate::execution_topology_metrics) fn reduce_lifecycle_rollup(
        &self,
        _horizon: &ObservabilityHorizonV1,
    ) -> Result<
        (
            ExecutionTopologyLifecycleRollupV1,
            ExecutionTopologyLifecycleCarryV1,
        ),
        ExecutionTopologyRollupStateErrorV1,
    > {
        let mut aggregate = ExecutionTopologyLifecycleRollupV1::default();
        let mut carry = ExecutionTopologyLifecycleCarryV1::default();
        for row in &self.integrations {
            if row.phase != IntegrationPhaseV1::NativeIntegratedObserved {
                continue;
            }
            aggregate.merge_eligible = aggregate.merge_eligible.saturating_add(1);
            if row.coverage != CoverageStateV1::Known {
                aggregate.merge_unknown = aggregate.merge_unknown.saturating_add(1);
                continue;
            }
            let kind = ExecutionIntegrationKindV1::from(row.operation);
            let outcome = ExecutionIntegrationOutcomeV1::from(row.result);
            let entry = aggregate.merge_cells.entry((kind, outcome)).or_default();
            *entry = entry.saturating_add(1);
            let totals = aggregate.merge_totals.entry(kind).or_default();
            totals.0 = totals.0.saturating_add(1);
            totals.1 = totals
                .1
                .saturating_add(u64::from(row.result == IntegrationResultV1::Succeeded));
        }
        for (trace, row) in &self.stack_drifts {
            carry.stack_drifts.insert(
                protected_key("execution-topology.stack-drift", trace)?,
                row.clone(),
            );
        }
        for row in &self.reruns {
            aggregate.rerun_eligible = aggregate.rerun_eligible.saturating_add(row.eligible);
            if row.coverage != CoverageStateV1::Known {
                aggregate.rerun_unknown = aggregate.rerun_unknown.saturating_add(row.eligible);
                continue;
            }
            let key = (row.source.into(), row.cause.into());
            let entry = aggregate.rerun_cells.entry(key).or_default();
            *entry = entry.saturating_add(row.linked);
            let eligible_entry = aggregate.rerun_eligible_cells.entry(key).or_default();
            *eligible_entry = eligible_entry.saturating_add(row.eligible);
            let totals = aggregate.rerun_totals.entry(row.source.into()).or_default();
            totals.0 = totals.0.saturating_add(row.eligible);
            totals.1 = totals.1.saturating_add(row.linked);
        }
        for row in &self.fanout {
            aggregate.delivery_attempted =
                aggregate.delivery_attempted.saturating_add(row.attempted);
            if row.coverage != CoverageStateV1::Known {
                aggregate.delivery_unknown =
                    aggregate.delivery_unknown.saturating_add(row.attempted);
                continue;
            }
            let totals = aggregate
                .delivery_totals
                .entry(row.surface.into())
                .or_insert([0; 5]);
            totals[0] = totals[0].saturating_add(row.attempted);
            totals[1] = totals[1].saturating_add(row.delivered);
            totals[2] = totals[2].saturating_add(row.deduplicated);
            totals[3] = totals[3].saturating_add(row.dropped);
            totals[4] = totals[4].saturating_add(row.unknown);
            aggregate.delivery_completed = aggregate
                .delivery_completed
                .saturating_add(row.delivered)
                .saturating_add(row.deduplicated);
            aggregate.delivery_dropped = aggregate.delivery_dropped.saturating_add(row.dropped);
            aggregate.delivery_unknown = aggregate.delivery_unknown.saturating_add(row.unknown);
        }
        aggregate.github_stack_capability = self.github_stack_capability.clone();
        let mut latest_blocked: BTreeMap<&str, (u32, Option<&BlockedRowV1>, i64)> = BTreeMap::new();
        for row in &self.blocked {
            match latest_blocked.get(row.receipt_ref.as_str()) {
                Some((revision, _, _)) if *revision > row.revision => {}
                Some((revision, existing, existing_time)) if *revision == row.revision => {
                    if existing.is_some_and(|existing| existing != row) {
                        latest_blocked.insert(
                            row.receipt_ref.as_str(),
                            (
                                row.revision,
                                None,
                                (*existing_time).max(row.event_time_micros),
                            ),
                        );
                    }
                }
                _ => {
                    latest_blocked.insert(
                        row.receipt_ref.as_str(),
                        (row.revision, Some(row), row.event_time_micros),
                    );
                }
            }
        }
        for (receipt, (revision, row, event_time_micros)) in latest_blocked {
            let key = protected_key("execution-topology.blocked", receipt)?;
            let row = row.map(|row| {
                let mut row = row.clone();
                row.receipt_ref = key.clone();
                row
            });
            carry.blocked.insert(
                key,
                LifecycleBlockedCandidateV1 {
                    revision,
                    row,
                    created_at_micros: event_time_micros,
                },
            );
        }
        for (receipt, (revision, row, event_time_micros)) in &self.leaks {
            let key = protected_key("execution-topology.leak", receipt)?;
            carry.leaks.insert(
                key,
                LifecycleLeakCandidateV1 {
                    revision: *revision,
                    row: *row,
                    created_at_micros: *event_time_micros,
                },
            );
        }
        check_carry(&carry)?;
        Ok((aggregate, carry))
    }
}

impl ExecutionTopologyLifecycleRollupV1 {
    pub(in crate::execution_topology_metrics) fn validate(
        &self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        let merge_cells = self
            .merge_cells
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        let merge_totals = self
            .merge_totals
            .values()
            .fold((0u64, 0u64), |totals, row| {
                (
                    totals.0.saturating_add(row.0),
                    totals.1.saturating_add(row.1),
                )
            });
        let stack_drift_cells = checked_sum(self.stack_drift_cells.values().copied());
        let rerun_cells = self
            .rerun_cells
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        let rerun_totals = self
            .rerun_totals
            .values()
            .fold((0u64, 0u64), |totals, row| {
                (
                    totals.0.saturating_add(row.0),
                    totals.1.saturating_add(row.1),
                )
            });
        let blocked_observed_by_cause = self
            .blocked_observed_by_cause
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        let rerun_eligible_cells = self
            .rerun_eligible_cells
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        let leak_cells = self
            .leak_cells
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        let delivery_attempted = self
            .delivery_totals
            .values()
            .map(|totals| totals[0])
            .fold(0u64, u64::saturating_add);
        if merge_cells.saturating_add(self.merge_unknown) != self.merge_eligible
            || merge_totals.0 != merge_cells
            || merge_totals.1 > merge_totals.0
            || !merge_dimensions_match(self)
            || stack_drift_cells.and_then(|observed| observed.checked_add(self.stack_drift_unknown))
                != Some(self.stack_drift_eligible)
            || blocked_observed_by_cause != self.blocked_observed
            || self
                .blocked_observed
                .saturating_add(self.blocked_censored)
                .saturating_add(self.blocked_unknown)
                != self.blocked_eligible
            || rerun_totals.0.saturating_add(self.rerun_unknown) != self.rerun_eligible
            || rerun_eligible_cells.saturating_add(self.rerun_unknown) != self.rerun_eligible
            || self.rerun_cells.iter().any(|(key, linked)| {
                *linked > self.rerun_eligible_cells.get(key).copied().unwrap_or(0)
            })
            || rerun_totals.1 != rerun_cells
            || rerun_totals.1 > rerun_totals.0
            || !rerun_dimensions_match(self)
            || leak_cells.saturating_add(self.leak_unknown) != self.leak_eligible
            || delivery_attempted.saturating_add(self.delivery_unknown) != self.delivery_attempted
            || self
                .delivery_completed
                .saturating_add(self.delivery_dropped)
                .saturating_add(self.delivery_unknown)
                != self.delivery_attempted
            || !delivery_dimensions_match(self)
            || !valid_interval_union(&self.blocked_union)
            || self
                .blocked_cause_unions
                .values()
                .any(|intervals| !valid_interval_union(intervals))
            || !blocked_cause_unions_are_subsets(self)
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn validate_for_horizon(
        &self,
        horizon: &ObservabilityHorizonV1,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        self.validate()?;
        let Some(row) = &self.github_stack_capability else {
            return Ok(());
        };
        let digest = canonical_sha256(&(
            row.capability,
            row.standard_git_fallback_available,
            row.other_forge_fallback_available,
            row.coverage,
        ))
        .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)?;
        if row.event_time_micros < horizon.since_micros
            || row.event_time_micros >= horizon.until_micros
            || row.observation_time_micros < row.event_time_micros
            || row.content_digest != digest.as_str()
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn merge(
        &mut self,
        other: Self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        for (key, value) in other.merge_cells {
            let entry = self.merge_cells.entry(key).or_default();
            *entry = entry.saturating_add(value);
        }
        for (key, (eligible, succeeded)) in other.merge_totals {
            let totals = self.merge_totals.entry(key).or_default();
            totals.0 = totals.0.saturating_add(eligible);
            totals.1 = totals.1.saturating_add(succeeded);
        }
        self.merge_eligible = self.merge_eligible.saturating_add(other.merge_eligible);
        self.merge_unknown = self.merge_unknown.saturating_add(other.merge_unknown);
        for (key, value) in other.stack_drift_cells {
            let entry = self.stack_drift_cells.entry(key).or_default();
            *entry = entry.saturating_add(value);
        }
        self.stack_drift_eligible = self
            .stack_drift_eligible
            .saturating_add(other.stack_drift_eligible);
        self.stack_drift_unknown = self
            .stack_drift_unknown
            .saturating_add(other.stack_drift_unknown);
        merge_segments(&mut self.blocked_union, &other.blocked_union)?;
        for (cause, intervals) in other.blocked_cause_unions {
            merge_segments(
                self.blocked_cause_unions.entry(cause).or_default(),
                &intervals,
            )?;
        }
        for (cause, observed) in other.blocked_observed_by_cause {
            let entry = self.blocked_observed_by_cause.entry(cause).or_default();
            *entry = entry.saturating_add(observed);
        }
        self.blocked_eligible = self.blocked_eligible.saturating_add(other.blocked_eligible);
        self.blocked_observed = self.blocked_observed.saturating_add(other.blocked_observed);
        self.blocked_censored = self.blocked_censored.saturating_add(other.blocked_censored);
        self.blocked_unknown = self.blocked_unknown.saturating_add(other.blocked_unknown);
        for (key, value) in other.rerun_cells {
            let entry = self.rerun_cells.entry(key).or_default();
            *entry = entry.saturating_add(value);
        }
        for (key, eligible) in other.rerun_eligible_cells {
            let entry = self.rerun_eligible_cells.entry(key).or_default();
            *entry = entry.saturating_add(eligible);
        }
        for (key, (eligible, linked)) in other.rerun_totals {
            let totals = self.rerun_totals.entry(key).or_default();
            totals.0 = totals.0.saturating_add(eligible);
            totals.1 = totals.1.saturating_add(linked);
        }
        self.rerun_eligible = self.rerun_eligible.saturating_add(other.rerun_eligible);
        self.rerun_unknown = self.rerun_unknown.saturating_add(other.rerun_unknown);
        for (key, value) in other.leak_cells {
            let entry = self.leak_cells.entry(key).or_default();
            *entry = entry.saturating_add(value);
        }
        self.leak_eligible = self.leak_eligible.saturating_add(other.leak_eligible);
        self.leak_unknown = self.leak_unknown.saturating_add(other.leak_unknown);
        for (surface, incoming) in other.delivery_totals {
            let totals = self.delivery_totals.entry(surface).or_insert([0; 5]);
            for index in 0..5 {
                totals[index] = totals[index].saturating_add(incoming[index]);
            }
        }
        self.delivery_attempted = self
            .delivery_attempted
            .saturating_add(other.delivery_attempted);
        self.delivery_completed = self
            .delivery_completed
            .saturating_add(other.delivery_completed);
        self.delivery_dropped = self.delivery_dropped.saturating_add(other.delivery_dropped);
        self.delivery_unknown = self.delivery_unknown.saturating_add(other.delivery_unknown);
        if other
            .github_stack_capability
            .as_ref()
            .is_some_and(|incoming| {
                self.github_stack_capability
                    .as_ref()
                    .is_none_or(|current| github_later(incoming, current))
            })
        {
            self.github_stack_capability = other.github_stack_capability;
        }
        Ok(())
    }
}

impl ExecutionTopologyLifecycleCarryV1 {
    pub(in crate::execution_topology_metrics) fn validate(
        &self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        check_carry(self)?;
        if self
            .stack_drifts
            .iter()
            .any(|(key, row)| !protected_key_is_valid(key) || !valid_stack_drift_row(row))
            || self.blocked.iter().any(|(key, candidate)| {
                !protected_key_is_valid(key)
                    || candidate.row.as_ref().is_some_and(|row| {
                        row.receipt_ref != *key
                            || row.revision != candidate.revision
                            || row.event_time_micros != candidate.created_at_micros
                    })
            })
            || self.leaks.iter().any(|(key, candidate)| {
                !protected_key_is_valid(key)
                    || candidate
                        .row
                        .is_some_and(|row| row.event_time_micros != candidate.created_at_micros)
            })
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn event_times_within(
        &self,
        since_micros: i64,
        until_micros: i64,
    ) -> bool {
        self.stack_drifts.values().all(|row| {
            row.event_time_micros >= since_micros && row.event_time_micros < until_micros
        }) && self.blocked.values().all(|candidate| {
            candidate.created_at_micros >= since_micros
                && candidate.created_at_micros < until_micros
        }) && self.leaks.values().all(|candidate| {
            candidate.created_at_micros >= since_micros
                && candidate.created_at_micros < until_micros
        })
    }

    pub(in crate::execution_topology_metrics) fn merge(
        &mut self,
        other: Self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        for (key, incoming) in other.stack_drifts {
            match self.stack_drifts.get_mut(&key) {
                None => {
                    self.stack_drifts.insert(key, incoming);
                }
                Some(existing) if !same_stack_drift_interval(&incoming, existing) => {
                    return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
                }
                Some(existing) if stack_drift_later(&incoming, existing) => *existing = incoming,
                Some(_) => {}
            }
        }
        for (key, incoming) in other.blocked {
            match self.blocked.get_mut(&key) {
                None => {
                    self.blocked.insert(key, incoming);
                }
                Some(existing) if existing.revision > incoming.revision => {}
                Some(existing) if existing.revision < incoming.revision => *existing = incoming,
                Some(existing) => {
                    if existing.row != incoming.row {
                        existing.row = None;
                    }
                    existing.created_at_micros =
                        existing.created_at_micros.max(incoming.created_at_micros);
                }
            }
        }
        for (key, incoming) in other.leaks {
            match self.leaks.get_mut(&key) {
                None => {
                    self.leaks.insert(key, incoming);
                }
                Some(existing) if existing.revision > incoming.revision => {}
                Some(existing) if existing.revision < incoming.revision => *existing = incoming,
                Some(existing) => {
                    if existing.row != incoming.row {
                        existing.row = None;
                    }
                    existing.created_at_micros =
                        existing.created_at_micros.max(incoming.created_at_micros);
                }
            }
        }
        check_carry(self)
    }
}

fn protected_key_is_valid(reference: &str) -> bool {
    reference.len() == 71
        && reference.starts_with("sha256:")
        && reference[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_interval_union(intervals: &[(i64, i64)]) -> bool {
    intervals.len() <= MAX_EXECUTION_TOPOLOGY_BLOCKED_UNION_SEGMENTS_V1
        && intervals.iter().all(|(start, end)| end >= start)
        && intervals.windows(2).all(|rows| rows[0].1 < rows[1].0)
}

fn valid_stack_drift_row(row: &StackDriftRowV1) -> bool {
    let payload = WorkStackDriftObservedV1 {
        kind: row.kind,
        state: row.state,
        first_observed_micros: row.first_observed_micros,
        terminal_micros: row.terminal_micros,
        age_bucket: row.age_bucket,
        coverage: row.coverage,
    };
    let endpoint = row.terminal_micros.unwrap_or(row.event_time_micros);
    let duration = endpoint
        .checked_sub(row.first_observed_micros)
        .and_then(|micros| u64::try_from(micros).ok());
    let digest = canonical_sha256(&(
        row.kind,
        row.state,
        row.first_observed_micros,
        row.terminal_micros,
        row.age_bucket,
        row.coverage,
    ));
    payload.validate().is_ok()
        && row.observation_time_micros >= row.event_time_micros
        && row.event_time_micros >= endpoint
        && duration.is_some_and(|duration| duration_bucket(duration) == row.age_bucket)
        && digest.is_ok_and(|digest| digest.as_str() == row.content_digest)
}

const fn duration_bucket(micros: u64) -> DurationBucketV1 {
    const MINUTE: u64 = 60_000_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match micros {
        value if value < MINUTE => DurationBucketV1::Under1m,
        value if value < 5 * MINUTE => DurationBucketV1::From1mTo5m,
        value if value < 15 * MINUTE => DurationBucketV1::From5mTo15m,
        value if value < HOUR => DurationBucketV1::From15mTo1h,
        value if value < 4 * HOUR => DurationBucketV1::From1hTo4h,
        value if value < DAY => DurationBucketV1::From4hTo24h,
        value if value < 7 * DAY => DurationBucketV1::From1dTo7d,
        _ => DurationBucketV1::Over7d,
    }
}

fn merge_dimensions_match(rollup: &ExecutionTopologyLifecycleRollupV1) -> bool {
    rollup
        .merge_cells
        .keys()
        .all(|(kind, _)| rollup.merge_totals.contains_key(kind))
        && rollup
            .merge_totals
            .iter()
            .all(|(kind, (eligible, succeeded))| {
                let cells = checked_sum(
                    rollup
                        .merge_cells
                        .iter()
                        .filter_map(|((cell_kind, _), value)| {
                            (cell_kind == kind).then_some(*value)
                        }),
                );
                cells == Some(*eligible)
                    && rollup
                        .merge_cells
                        .get(&(*kind, ExecutionIntegrationOutcomeV1::Succeeded))
                        .copied()
                        .unwrap_or(0)
                        == *succeeded
            })
}

fn rerun_dimensions_match(rollup: &ExecutionTopologyLifecycleRollupV1) -> bool {
    rollup
        .rerun_cells
        .keys()
        .chain(rollup.rerun_eligible_cells.keys())
        .all(|(source, _)| rollup.rerun_totals.contains_key(source))
        && rollup
            .rerun_totals
            .iter()
            .all(|(source, (eligible, linked))| {
                checked_sum(rollup.rerun_eligible_cells.iter().filter_map(
                    |((cell_source, _), value)| (cell_source == source).then_some(*value),
                )) == Some(*eligible)
                    && checked_sum(rollup.rerun_cells.iter().filter_map(
                        |((cell_source, _), value)| (cell_source == source).then_some(*value),
                    )) == Some(*linked)
            })
}

fn delivery_dimensions_match(rollup: &ExecutionTopologyLifecycleRollupV1) -> bool {
    let cells_are_complete = rollup
        .delivery_totals
        .values()
        .all(|totals| checked_sum(totals[1..].iter().copied()) == Some(totals[0]));
    let attempted = checked_sum(rollup.delivery_totals.values().map(|totals| totals[0]));
    let completed = checked_sum(
        rollup
            .delivery_totals
            .values()
            .flat_map(|totals| [totals[1], totals[2]]),
    );
    let dropped = checked_sum(rollup.delivery_totals.values().map(|totals| totals[3]));
    let known_unknown = checked_sum(rollup.delivery_totals.values().map(|totals| totals[4]));
    cells_are_complete
        && completed == Some(rollup.delivery_completed)
        && dropped == Some(rollup.delivery_dropped)
        && attempted
            .and_then(|attempted| rollup.delivery_attempted.checked_sub(attempted))
            .zip(known_unknown)
            .and_then(|(unavailable, known)| unavailable.checked_add(known))
            == Some(rollup.delivery_unknown)
}

fn blocked_cause_unions_are_subsets(rollup: &ExecutionTopologyLifecycleRollupV1) -> bool {
    rollup
        .blocked_cause_unions
        .iter()
        .all(|(cause, intervals)| {
            rollup
                .blocked_observed_by_cause
                .get(cause)
                .copied()
                .unwrap_or(0)
                > 0
                && intervals.iter().all(|(start, end)| {
                    rollup
                        .blocked_union
                        .iter()
                        .any(|(total_start, total_end)| total_start <= start && end <= total_end)
                })
        })
        && rollup
            .blocked_observed_by_cause
            .iter()
            .all(|(cause, observed)| {
                *observed == 0 || rollup.blocked_cause_unions.contains_key(cause)
            })
}

fn checked_sum(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0u64, |total, value| total.checked_add(value))
}

fn github_later(
    incoming: &GitHubStackCapabilityRowV1,
    current: &GitHubStackCapabilityRowV1,
) -> bool {
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

pub(in crate::execution_topology_metrics) fn apply_carry_to_rollup(
    aggregate: &mut ExecutionTopologyLifecycleRollupV1,
    carry: &ExecutionTopologyLifecycleCarryV1,
) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
    for row in carry.stack_drifts.values() {
        aggregate.stack_drift_eligible = aggregate.stack_drift_eligible.saturating_add(1);
        if row.coverage != CoverageStateV1::Known {
            aggregate.stack_drift_unknown = aggregate.stack_drift_unknown.saturating_add(1);
            continue;
        }
        let key = (row.kind.into(), row.state.into(), row.age_bucket.into());
        let entry = aggregate.stack_drift_cells.entry(key).or_default();
        *entry = entry.saturating_add(1);
    }
    for candidate in carry.blocked.values() {
        aggregate.blocked_eligible = aggregate.blocked_eligible.saturating_add(1);
        match &candidate.row {
            None => aggregate.blocked_unknown = aggregate.blocked_unknown.saturating_add(1),
            Some(row) if row.coverage != CoverageStateV1::Known => {
                aggregate.blocked_unknown = aggregate.blocked_unknown.saturating_add(1)
            }
            Some(row) => match row.valid_until_micros {
                Some(until) if until >= row.valid_from_micros => {
                    aggregate.blocked_observed = aggregate.blocked_observed.saturating_add(1);
                    let cause = ExecutionBlockedCauseV1::from(row.cause);
                    let entry = aggregate
                        .blocked_observed_by_cause
                        .entry(cause)
                        .or_default();
                    *entry = entry.saturating_add(1);
                    fold_interval(aggregate, cause, (row.valid_from_micros, until))?;
                }
                _ => aggregate.blocked_censored = aggregate.blocked_censored.saturating_add(1),
            },
        }
    }
    for candidate in carry.leaks.values() {
        aggregate.leak_eligible = aggregate.leak_eligible.saturating_add(1);
        let Some(row) = candidate.row else {
            aggregate.leak_unknown = aggregate.leak_unknown.saturating_add(1);
            continue;
        };
        if row.coverage != CoverageStateV1::Known {
            aggregate.leak_unknown = aggregate.leak_unknown.saturating_add(1);
            continue;
        }
        let key = (row.kind.into(), row.recovery.into());
        let entry = aggregate.leak_cells.entry(key).or_default();
        *entry = entry.saturating_add(1);
    }
    Ok(())
}
