use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use super::{StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReaderLaneV1 {
    General,
    ReservedHealth,
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
