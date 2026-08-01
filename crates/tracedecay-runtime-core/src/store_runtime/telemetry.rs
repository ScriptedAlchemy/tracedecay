//! Pure projection of daemon-owned runtime health into bounded telemetry data.
//!
//! `ShardRuntime` and the registry own collection. This module only transforms
//! their already-captured, path-free values; it does not open a store, sample a
//! metrics backend, retain global state, or derive identity from a locator.
//!
//! Dead-code allowance lives on the parent `store_runtime` module until daemon
//! read surfaces consume the projection.

use std::cmp::Ordering;
use std::time::Duration;

use tracedecay_store::{
    AdmissionConfigV1, QueueBudgetV1, RuntimeMaintenanceStateV1, StoreRuntimeBindingV1, WalBudgetV1,
};

use super::registry::PhysicalRuntimeSnapshot;
use super::shard::{
    ShardRuntimeEvictionEligibility, ShardRuntimeHealth, ShardRuntimeHealthSnapshot,
    ShardRuntimeObservation,
};

/// Maximum number of per-shard detail rows returned by one projection.
///
/// The aggregate still covers every supplied inventory entry and reports the
/// number of omitted detail rows.
pub const MAX_PROJECTED_RUNTIME_SHARDS: usize = 64;

/// One registry inventory item captured before telemetry projection.
///
/// Both values must describe the same observation interval. The projection
/// preserves the registry/shard eviction decision rather than recomputing it
/// from individual counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRegistryInventoryEntry {
    pub health: ShardRuntimeHealthSnapshot,
    pub eviction: ShardRuntimeEvictionEligibility,
    pub physical: PhysicalRuntimeSnapshot,
}

impl From<ShardRuntimeObservation> for RuntimeRegistryInventoryEntry {
    fn from(observation: ShardRuntimeObservation) -> Self {
        Self {
            health: observation.health,
            eviction: observation.eviction,
            physical: PhysicalRuntimeSnapshot::default(),
        }
    }
}

/// Path-free registry inventory supplied by the daemon after it has collected
/// shard health snapshots.
///
/// `global_queued_bytes` is an explicit registry observation. It must not be
/// synthesized by summing shard queues because global admission can include
/// work not yet associated with a shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRegistryInventory {
    pub admission: AdmissionConfigV1,
    pub global_queued_bytes: u64,
    pub entries: Vec<RuntimeRegistryInventoryEntry>,
}

/// Counted leases held by a shard at one health-snapshot instant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShardRuntimeLeaseCounts {
    pub general_readers: u32,
    pub health_readers: u32,
    pub snapshots: u32,
    pub watchers: u32,
    pub schedulers: u32,
    pub clients: u32,
}

impl ShardRuntimeLeaseCounts {
    fn from_health(health: &ShardRuntimeHealthSnapshot) -> Self {
        Self {
            general_readers: health.general_reader_leases,
            health_readers: health.health_reader_leases,
            snapshots: health.snapshot_leases,
            watchers: health.watcher_leases,
            schedulers: health.scheduler_leases,
            clients: health.client_leases,
        }
    }
}

/// Driver-neutral telemetry detail for one daemon-owned runtime publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardRuntimeTelemetry {
    /// The source binding carries the canonical shard, incarnation, and epoch.
    pub binding: StoreRuntimeBindingV1,
    pub state: RuntimeMaintenanceStateV1,
    pub queue_budget: QueueBudgetV1,
    pub global_queue_budget_bytes: u64,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub writer_present: bool,
    pub physical_reader_handles: u32,
    pub leases: ShardRuntimeLeaseCounts,
    pub wal_bytes: u64,
    pub wal_budget: WalBudgetV1,
    pub memory_estimate_bytes: u64,
    pub health: ShardRuntimeHealth,
    pub pinned_profile: bool,
    pub idle_for_ms: u64,
    pub eviction_eligible: bool,
    pub eviction_blocker_count: u32,
}

/// Fixed-shape, saturating counts of runtime states.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStateCounts {
    pub closed: u32,
    pub opening: u32,
    pub ready: u32,
    pub draining: u32,
    pub exclusive_maintenance: u32,
    pub reopening: u32,
    pub faulted: u32,
}

impl RuntimeStateCounts {
    fn observe(&mut self, state: RuntimeMaintenanceStateV1) {
        let count = match state {
            RuntimeMaintenanceStateV1::Closed => &mut self.closed,
            RuntimeMaintenanceStateV1::Opening => &mut self.opening,
            RuntimeMaintenanceStateV1::Ready => &mut self.ready,
            RuntimeMaintenanceStateV1::Draining => &mut self.draining,
            RuntimeMaintenanceStateV1::ExclusiveMaintenance => &mut self.exclusive_maintenance,
            RuntimeMaintenanceStateV1::Reopening => &mut self.reopening,
            RuntimeMaintenanceStateV1::Faulted => &mut self.faulted,
        };
        *count = count.saturating_add(1);
    }
}

/// Fixed-shape, saturating counts of observed health states.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeHealthCounts {
    pub unknown: u32,
    pub healthy: u32,
    pub degraded: u32,
    pub faulted: u32,
}

impl RuntimeHealthCounts {
    fn observe(&mut self, health: ShardRuntimeHealth) {
        let count = match health {
            ShardRuntimeHealth::Unknown => &mut self.unknown,
            ShardRuntimeHealth::Healthy => &mut self.healthy,
            ShardRuntimeHealth::Degraded => &mut self.degraded,
            ShardRuntimeHealth::Faulted => &mut self.faulted,
        };
        *count = count.saturating_add(1);
    }
}

/// Bounded aggregate facts for every entry in a registry inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeTelemetryAggregate {
    pub inventory_shards: u32,
    pub returned_shards: u32,
    pub omitted_shards: u32,
    pub states: RuntimeStateCounts,
    pub health: RuntimeHealthCounts,
    pub pinned_profiles: u32,
    pub eviction_eligible: u32,
    pub writer_present: u32,
    pub physical_reader_handles: u64,
    pub queued_operations: u64,
    pub queued_bytes: u64,
    pub general_reader_leases: u64,
    pub health_reader_leases: u64,
    pub snapshot_leases: u64,
    pub watcher_leases: u64,
    pub scheduler_leases: u64,
    pub client_leases: u64,
    pub total_leases: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub global_queued_bytes: u64,
}

impl RuntimeTelemetryAggregate {
    fn observe(&mut self, entry: &RuntimeRegistryInventoryEntry) {
        let health = &entry.health;
        self.states.observe(health.state);
        self.health.observe(health.health);
        self.pinned_profiles = self
            .pinned_profiles
            .saturating_add(count_if(health.pinned_profile));
        self.eviction_eligible = self
            .eviction_eligible
            .saturating_add(count_if(entry.eviction.is_eligible()));
        self.writer_present = self
            .writer_present
            .saturating_add(count_if(health.writer_present));
        self.physical_reader_handles = self
            .physical_reader_handles
            .saturating_add(u64::from(entry.physical.reader_handles));
        self.queued_operations = self
            .queued_operations
            .saturating_add(u64::from(health.queued_operations));
        self.queued_bytes = self.queued_bytes.saturating_add(health.queued_bytes);
        self.general_reader_leases = self
            .general_reader_leases
            .saturating_add(u64::from(health.general_reader_leases));
        self.health_reader_leases = self
            .health_reader_leases
            .saturating_add(u64::from(health.health_reader_leases));
        self.snapshot_leases = self
            .snapshot_leases
            .saturating_add(u64::from(health.snapshot_leases));
        self.watcher_leases = self
            .watcher_leases
            .saturating_add(u64::from(health.watcher_leases));
        self.scheduler_leases = self
            .scheduler_leases
            .saturating_add(u64::from(health.scheduler_leases));
        self.client_leases = self
            .client_leases
            .saturating_add(u64::from(health.client_leases));
        let lease_total = u64::from(health.general_reader_leases)
            .saturating_add(u64::from(health.health_reader_leases))
            .saturating_add(u64::from(health.snapshot_leases))
            .saturating_add(u64::from(health.watcher_leases))
            .saturating_add(u64::from(health.scheduler_leases))
            .saturating_add(u64::from(health.client_leases));
        self.total_leases = self.total_leases.saturating_add(lease_total);
        self.wal_bytes = self.wal_bytes.saturating_add(health.wal_bytes);
        self.memory_estimate_bytes = self
            .memory_estimate_bytes
            .saturating_add(health.memory_estimate_bytes);
    }
}

/// Deterministically ordered bounded telemetry details plus full aggregates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeTelemetryProjection {
    pub per_shard_queue_budget: QueueBudgetV1,
    pub global_queue_budget_bytes: u64,
    pub wal_budget: WalBudgetV1,
    pub shards: Vec<ShardRuntimeTelemetry>,
    pub aggregate: RuntimeTelemetryAggregate,
}

/// Projects a registry inventory with the standard per-shard detail bound.
pub fn project_runtime_telemetry(
    inventory: &RuntimeRegistryInventory,
) -> RuntimeTelemetryProjection {
    project_runtime_telemetry_with_limit(inventory, MAX_PROJECTED_RUNTIME_SHARDS)
}

fn project_runtime_telemetry_with_limit(
    inventory: &RuntimeRegistryInventory,
    max_shards: usize,
) -> RuntimeTelemetryProjection {
    let mut entries = inventory.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| compare_binding(&left.health.binding, &right.health.binding));

    let returned_len = entries.len().min(max_shards);
    let mut aggregate = RuntimeTelemetryAggregate {
        inventory_shards: bounded_count(entries.len()),
        returned_shards: bounded_count(returned_len),
        omitted_shards: bounded_count(entries.len().saturating_sub(returned_len)),
        global_queued_bytes: inventory.global_queued_bytes,
        ..RuntimeTelemetryAggregate::default()
    };
    for entry in &entries {
        aggregate.observe(entry);
    }

    let shards = entries
        .into_iter()
        .take(returned_len)
        .map(|entry| project_shard(entry, &inventory.admission))
        .collect();

    RuntimeTelemetryProjection {
        per_shard_queue_budget: inventory.admission.per_shard_queue.clone(),
        global_queue_budget_bytes: inventory.admission.global_queue_max_bytes,
        wal_budget: inventory.admission.wal.clone(),
        shards,
        aggregate,
    }
}

fn project_shard(
    entry: &RuntimeRegistryInventoryEntry,
    admission: &AdmissionConfigV1,
) -> ShardRuntimeTelemetry {
    let health = &entry.health;
    ShardRuntimeTelemetry {
        binding: health.binding.clone(),
        state: health.state,
        queue_budget: admission.per_shard_queue.clone(),
        global_queue_budget_bytes: admission.global_queue_max_bytes,
        queued_operations: health.queued_operations,
        queued_bytes: health.queued_bytes,
        writer_present: health.writer_present,
        physical_reader_handles: entry.physical.reader_handles,
        leases: ShardRuntimeLeaseCounts::from_health(health),
        wal_bytes: health.wal_bytes,
        wal_budget: admission.wal.clone(),
        memory_estimate_bytes: health.memory_estimate_bytes,
        health: health.health,
        pinned_profile: health.pinned_profile,
        idle_for_ms: duration_millis(entry.eviction.idle_for),
        eviction_eligible: entry.eviction.is_eligible(),
        eviction_blocker_count: bounded_count(entry.eviction.blockers.len()),
    }
}

fn compare_binding(left: &StoreRuntimeBindingV1, right: &StoreRuntimeBindingV1) -> Ordering {
    left.shard_id
        .cmp(&right.shard_id)
        .then_with(|| left.incarnation.cmp(&right.incarnation))
        .then_with(|| left.authority_epoch.cmp(&right.authority_epoch))
}

fn bounded_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

const fn count_if(value: bool) -> u32 {
    if value { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_store::{StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1};

    use super::*;
    use crate::store_runtime::shard::{
        ShardRuntime, ShardRuntimeEvictionBlocker, ShardRuntimeLeaseKind,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn binding(project: &str, incarnation: u64, epoch: u64) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                id::<BrainId>("brain.runtime-telemetry"),
                id::<UserProfileId>("profile.runtime-telemetry"),
                id::<ProjectId>(project),
            ),
            StoreIncarnationV1::new(incarnation).unwrap(),
            StoreAuthorityEpochV1::new(epoch).unwrap(),
        )
    }

    fn fixture_health(
        project: &str,
        incarnation: u64,
        state: RuntimeMaintenanceStateV1,
    ) -> ShardRuntimeHealthSnapshot {
        ShardRuntimeHealthSnapshot {
            binding: binding(project, incarnation, 7),
            state,
            pinned_profile: false,
            writer_present: false,
            queued_operations: 3,
            queued_bytes: 768,
            general_reader_leases: 2,
            health_reader_leases: 1,
            snapshot_leases: 1,
            watcher_leases: 1,
            scheduler_leases: 1,
            client_leases: 1,
            wal_bytes: 8_192,
            memory_estimate_bytes: 16_384,
            idle_for: Duration::from_mins(1),
            health: ShardRuntimeHealth::Healthy,
        }
    }

    fn entry(
        health: ShardRuntimeHealthSnapshot,
        blockers: Vec<ShardRuntimeEvictionBlocker>,
    ) -> RuntimeRegistryInventoryEntry {
        RuntimeRegistryInventoryEntry {
            eviction: ShardRuntimeEvictionEligibility {
                idle_for: health.idle_for,
                blockers,
            },
            health,
            physical: PhysicalRuntimeSnapshot::default(),
        }
    }

    fn inventory(entries: Vec<RuntimeRegistryInventoryEntry>) -> RuntimeRegistryInventory {
        RuntimeRegistryInventory {
            admission: AdmissionConfigV1::default(),
            global_queued_bytes: 2_048,
            entries,
        }
    }

    #[test]
    fn projection_is_deterministic_bounded_and_aggregates_full_inventory() {
        let last = fixture_health("project.z", 1, RuntimeMaintenanceStateV1::Ready);
        let mut first = fixture_health("project.a", 2, RuntimeMaintenanceStateV1::Faulted);
        first.health = ShardRuntimeHealth::Faulted;
        let middle = fixture_health("project.m", 1, RuntimeMaintenanceStateV1::Ready);
        let inventory = inventory(vec![
            entry(last, vec![]),
            entry(
                first.clone(),
                vec![ShardRuntimeEvictionBlocker::NotReady {
                    state: RuntimeMaintenanceStateV1::Faulted,
                }],
            ),
            entry(middle.clone(), vec![]),
        ]);

        let projection = project_runtime_telemetry_with_limit(&inventory, 2);

        assert_eq!(projection.shards.len(), 2);
        assert_eq!(projection.shards[0].binding, first.binding);
        assert_eq!(projection.shards[1].binding, middle.binding);
        assert_eq!(projection.aggregate.inventory_shards, 3);
        assert_eq!(projection.aggregate.returned_shards, 2);
        assert_eq!(projection.aggregate.omitted_shards, 1);
        assert_eq!(projection.aggregate.states.ready, 2);
        assert_eq!(projection.aggregate.states.faulted, 1);
        assert_eq!(projection.aggregate.health.healthy, 2);
        assert_eq!(projection.aggregate.health.faulted, 1);
        assert_eq!(projection.aggregate.queued_operations, 9);
        assert_eq!(projection.aggregate.queued_bytes, 2_304);
        assert_eq!(projection.aggregate.total_leases, 21);
        assert_eq!(projection.aggregate.wal_bytes, 24_576);
        assert_eq!(projection.aggregate.memory_estimate_bytes, 49_152);
        assert_eq!(projection.aggregate.global_queued_bytes, 2_048);
        assert_eq!(
            projection.per_shard_queue_budget,
            inventory.admission.per_shard_queue
        );
        assert_eq!(
            projection.global_queue_budget_bytes,
            inventory.admission.global_queue_max_bytes
        );
    }

    #[test]
    fn projection_preserves_actual_shard_health_identity_and_eviction_decision() {
        let runtime = ShardRuntime::new(binding("project.live", 3, 9), true);
        runtime
            .transition(RuntimeMaintenanceStateV1::Opening)
            .unwrap();
        runtime
            .transition(RuntimeMaintenanceStateV1::Ready)
            .unwrap();
        runtime.record_storage_usage(65_536, 131_072);
        runtime.set_health(ShardRuntimeHealth::Degraded).unwrap();
        let queue = runtime.queue_work(4, 1_024).unwrap();
        let reader = runtime
            .acquire_lease(ShardRuntimeLeaseKind::GeneralReader)
            .unwrap();
        let health = runtime.health_snapshot();
        let eviction = runtime.eviction_eligibility_at(
            runtime.last_activity() + Duration::from_mins(1),
            Duration::from_mins(1),
        );
        let projection =
            project_runtime_telemetry(&inventory(vec![RuntimeRegistryInventoryEntry {
                health,
                eviction,
                physical: PhysicalRuntimeSnapshot::default(),
            }]));
        let shard = projection.shards.first().unwrap();

        assert_eq!(shard.binding, runtime.binding().clone());
        assert_eq!(shard.state, RuntimeMaintenanceStateV1::Ready);
        assert_eq!(shard.queued_operations, 4);
        assert_eq!(shard.queued_bytes, 1_024);
        assert_eq!(shard.leases.general_readers, 1);
        assert_eq!(shard.wal_bytes, 65_536);
        assert_eq!(shard.memory_estimate_bytes, 131_072);
        assert_eq!(shard.health, ShardRuntimeHealth::Degraded);
        assert!(shard.pinned_profile);
        assert!(!shard.eviction_eligible);
        assert!(shard.eviction_blocker_count >= 3);

        reader.release();
        queue.release();
    }

    #[test]
    fn aggregate_counts_saturate_instead_of_wrapping() {
        let mut first = fixture_health("project.one", 1, RuntimeMaintenanceStateV1::Ready);
        first.general_reader_leases = u32::MAX;
        let mut second = fixture_health("project.two", 1, RuntimeMaintenanceStateV1::Ready);
        second.general_reader_leases = 1;
        let projection = project_runtime_telemetry(&inventory(vec![
            entry(first, vec![]),
            entry(second, vec![]),
        ]));

        assert_eq!(
            projection.aggregate.general_reader_leases,
            u64::from(u32::MAX) + 1
        );
        assert_eq!(projection.aggregate.total_leases, u64::from(u32::MAX) + 11);
    }
}
