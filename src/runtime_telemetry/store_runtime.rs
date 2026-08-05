//! Wire projection for the daemon-owned store-runtime telemetry inventory.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistrySnapshot {
    pub inventory_shards: u32,
    pub returned_shards: u32,
    pub omitted_shards: u32,
    pub per_shard_queue_max_operations: u32,
    pub per_shard_queue_max_bytes: u64,
    pub global_queue_max_bytes: u64,
    pub wal_soft_limit_bytes: u64,
    pub wal_hard_limit_bytes: u64,
    pub aggregate: RuntimeRegistryAggregateSnapshot,
    pub shards: Vec<RuntimeRegistryShardSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistryAggregateSnapshot {
    pub ready: u32,
    pub opening: u32,
    pub draining: u32,
    pub exclusive_maintenance: u32,
    pub reopening: u32,
    pub faulted: u32,
    pub closed: u32,
    pub healthy: u32,
    pub degraded: u32,
    pub unknown_health: u32,
    pub pinned_profiles: u32,
    pub eviction_eligible: u32,
    pub writer_present: u32,
    pub physical_reader_handles: u64,
    pub general_reader_waiters: u64,
    pub health_reader_waiters: u64,
    pub writer_busy_events: u64,
    pub writer_telemetry_shards: u32,
    pub writer_telemetry_complete: bool,
    pub offered_operations: u64,
    pub admitted_operations: u64,
    pub completed_operations: u64,
    pub shed_operations: u64,
    pub retried_operations: u64,
    pub cancelled_operations: u64,
    pub deadline_exceeded_operations: u64,
    pub conflicted_operations: u64,
    pub committed_batches: u64,
    pub writer_queue_wait_micros: u64,
    pub writer_transaction_micros: u64,
    pub writer_error_events: u64,
    pub health_lane_services: u64,
    pub queued_operations: u64,
    pub queued_bytes: u64,
    pub total_leases: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub global_queued_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeRegistryShardSnapshot {
    pub shard: String,
    pub incarnation: u64,
    pub authority_epoch: u64,
    pub state: String,
    pub health: String,
    pub writer_present: bool,
    pub physical_reader_handles: u32,
    pub general_reader_waiters: u16,
    pub health_reader_waiters: u16,
    pub writer_busy_events: u64,
    pub writer: Option<RuntimeRegistryWriterSnapshot>,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub total_leases: u64,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub pinned_profile: bool,
    pub idle_for_ms: u64,
    pub eviction_eligible: bool,
    pub eviction_blocker_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RuntimeRegistryWriterSnapshot {
    pub offered_operations: u64,
    pub admitted_operations: u64,
    pub completed_operations: u64,
    pub shed_operations: u64,
    pub retried_operations: u64,
    pub cancelled_operations: u64,
    pub deadline_exceeded_operations: u64,
    pub conflicted_operations: u64,
    pub committed_batches: u64,
    pub queue_wait_micros: u64,
    pub transaction_micros: u64,
    pub error_events: u64,
    pub health_lane_services: u64,
    pub commit_sequence: u64,
}

impl RuntimeRegistrySnapshot {
    pub(super) fn from_projection(
        projection: crate::daemon::store_runtime::telemetry::RuntimeTelemetryProjection,
    ) -> Self {
        let aggregate = &projection.aggregate;
        let shards = projection
            .shards
            .iter()
            .map(RuntimeRegistryShardSnapshot::from_telemetry)
            .collect();
        Self {
            inventory_shards: aggregate.inventory_shards,
            returned_shards: aggregate.returned_shards,
            omitted_shards: aggregate.omitted_shards,
            per_shard_queue_max_operations: projection.per_shard_queue_budget.max_operations,
            per_shard_queue_max_bytes: projection.per_shard_queue_budget.max_bytes,
            global_queue_max_bytes: projection.global_queue_budget_bytes,
            wal_soft_limit_bytes: projection.wal_budget.soft_limit_bytes,
            wal_hard_limit_bytes: projection.wal_budget.hard_limit_bytes,
            aggregate: RuntimeRegistryAggregateSnapshot {
                ready: aggregate.states.ready,
                opening: aggregate.states.opening,
                draining: aggregate.states.draining,
                exclusive_maintenance: aggregate.states.exclusive_maintenance,
                reopening: aggregate.states.reopening,
                faulted: aggregate.states.faulted,
                closed: aggregate.states.closed,
                healthy: aggregate.health.healthy,
                degraded: aggregate.health.degraded,
                unknown_health: aggregate.health.unknown,
                pinned_profiles: aggregate.pinned_profiles,
                eviction_eligible: aggregate.eviction_eligible,
                writer_present: aggregate.writer_present,
                physical_reader_handles: aggregate.physical_reader_handles,
                general_reader_waiters: aggregate.general_reader_waiters,
                health_reader_waiters: aggregate.health_reader_waiters,
                writer_busy_events: aggregate.writer_busy_events,
                writer_telemetry_shards: aggregate.writer_telemetry_shards,
                writer_telemetry_complete: aggregate.writer_telemetry_complete,
                offered_operations: aggregate.offered_operations,
                admitted_operations: aggregate.admitted_operations,
                completed_operations: aggregate.completed_operations,
                shed_operations: aggregate.shed_operations,
                retried_operations: aggregate.retried_operations,
                cancelled_operations: aggregate.cancelled_operations,
                deadline_exceeded_operations: aggregate.deadline_exceeded_operations,
                conflicted_operations: aggregate.conflicted_operations,
                committed_batches: aggregate.committed_batches,
                writer_queue_wait_micros: aggregate.writer_queue_wait_micros,
                writer_transaction_micros: aggregate.writer_transaction_micros,
                writer_error_events: aggregate.writer_error_events,
                health_lane_services: aggregate.health_lane_services,
                queued_operations: aggregate.queued_operations,
                queued_bytes: aggregate.queued_bytes,
                total_leases: aggregate.total_leases,
                wal_bytes: aggregate.wal_bytes,
                memory_estimate_bytes: aggregate.memory_estimate_bytes,
                global_queued_bytes: aggregate.global_queued_bytes,
            },
            shards,
        }
    }
}

impl RuntimeRegistryShardSnapshot {
    fn from_telemetry(
        telemetry: &crate::daemon::store_runtime::telemetry::ShardRuntimeTelemetry,
    ) -> Self {
        Self {
            shard: format!("{:?}", telemetry.binding.shard_id.scope),
            incarnation: telemetry.binding.incarnation.get(),
            authority_epoch: telemetry.binding.authority_epoch.get(),
            state: runtime_state_label(telemetry.state).to_owned(),
            health: runtime_health_label(telemetry.health).to_owned(),
            writer_present: telemetry.writer_present,
            physical_reader_handles: telemetry.physical_reader_handles,
            general_reader_waiters: telemetry.general_reader_waiters,
            health_reader_waiters: telemetry.health_reader_waiters,
            writer_busy_events: telemetry.writer_busy_events,
            writer: telemetry
                .writer
                .map(|writer| RuntimeRegistryWriterSnapshot {
                    offered_operations: writer.offered_operations,
                    admitted_operations: writer.admitted_operations,
                    completed_operations: writer.completed_operations,
                    shed_operations: writer.shed_operations,
                    retried_operations: writer.retried_operations,
                    cancelled_operations: writer.cancelled_operations,
                    deadline_exceeded_operations: writer.deadline_exceeded_operations,
                    conflicted_operations: writer.conflicted_operations,
                    committed_batches: writer.committed_batches,
                    queue_wait_micros: writer.queue_wait_micros,
                    transaction_micros: writer.transaction_micros,
                    error_events: writer.error_events,
                    health_lane_services: writer.health_lane_services,
                    commit_sequence: writer.commit_sequence.0,
                }),
            queued_operations: telemetry.queued_operations,
            queued_bytes: telemetry.queued_bytes,
            total_leases: u64::from(telemetry.leases.general_readers)
                .saturating_add(u64::from(telemetry.leases.health_readers))
                .saturating_add(u64::from(telemetry.leases.snapshots))
                .saturating_add(u64::from(telemetry.leases.watchers))
                .saturating_add(u64::from(telemetry.leases.schedulers))
                .saturating_add(u64::from(telemetry.leases.clients)),
            wal_bytes: telemetry.wal_bytes,
            memory_estimate_bytes: telemetry.memory_estimate_bytes,
            pinned_profile: telemetry.pinned_profile,
            idle_for_ms: telemetry.idle_for_ms,
            eviction_eligible: telemetry.eviction_eligible,
            eviction_blocker_count: telemetry.eviction_blocker_count,
        }
    }
}

fn runtime_state_label(state: tracedecay_store::RuntimeMaintenanceStateV1) -> &'static str {
    match state {
        tracedecay_store::RuntimeMaintenanceStateV1::Closed => "closed",
        tracedecay_store::RuntimeMaintenanceStateV1::Opening => "opening",
        tracedecay_store::RuntimeMaintenanceStateV1::Ready => "ready",
        tracedecay_store::RuntimeMaintenanceStateV1::Draining => "draining",
        tracedecay_store::RuntimeMaintenanceStateV1::ExclusiveMaintenance => {
            "exclusive_maintenance"
        }
        tracedecay_store::RuntimeMaintenanceStateV1::Reopening => "reopening",
        tracedecay_store::RuntimeMaintenanceStateV1::Faulted => "faulted",
    }
}

fn runtime_health_label(
    health: crate::daemon::store_runtime::shard::ShardRuntimeHealth,
) -> &'static str {
    match health {
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Unknown => "unknown",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Healthy => "healthy",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Degraded => "degraded",
        crate::daemon::store_runtime::shard::ShardRuntimeHealth::Faulted => "faulted",
    }
}
