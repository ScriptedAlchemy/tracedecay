use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use super::{
    CommitSequenceV1, DurabilityClassV1, OperationPriorityV1, StoreAuthorityEpochV1,
    StoreIncarnationV1, StoreShardIdV1,
};

/// Queue accounting suitable for open-loop overload reporting.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmissionTelemetryV1 {
    pub offered_operations: u64,
    pub admitted_operations: u64,
    pub completed_operations: u64,
    pub shed_operations: u64,
    pub retried_operations: u64,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub global_queued_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommitTelemetryV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub commit_sequence: CommitSequenceV1,
    pub priority: OperationPriorityV1,
    pub durability: DurabilityClassV1,
    pub batch_operations: u32,
    pub batch_bytes: u64,
    pub queue_wait_micros: u64,
    pub transaction_micros: u64,
    pub committed_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReaderLaneV1 {
    General,
    ReservedHealth,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReaderTelemetryV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub general_active: u16,
    pub general_idle: u16,
    pub general_waiters: u32,
    pub health_active: bool,
    pub retained_snapshots: u32,
    pub longest_snapshot_age_ms: u64,
    pub wait_micros: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMaintenanceStateV1 {
    Closed,
    Opening,
    Ready,
    Draining,
    ExclusiveMaintenance,
    Reopening,
    Faulted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WalPressureV1 {
    Normal,
    SoftLimit,
    HardLimit,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceTelemetryV1 {
    pub shard_id: StoreShardIdV1,
    pub incarnation: StoreIncarnationV1,
    pub authority_epoch: StoreAuthorityEpochV1,
    pub state: RuntimeMaintenanceStateV1,
    pub wal_bytes: u64,
    pub wal_pressure: WalPressureV1,
    pub blocked_snapshots: u32,
    pub checkpoint_count: u64,
    pub checkpoint_busy_count: u64,
    pub last_checkpoint_at: Option<UtcMicros>,
}
