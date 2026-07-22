//! Lifecycle owner for one fully attached physical shard runtime.

mod doctor;
mod engine;
mod ports;

pub use doctor::{
    DoctorHealthError, DoctorHealthSnapshot, IntegrityResult, SqliteDoctorHealthLane, WalHealth,
};
pub use engine::{
    ShardRuntimeEngine, ShardRuntimeStartError, ShardRuntimeState, ShardRuntimeTelemetry,
};
pub use ports::{
    BackupControl, CheckpointControl, MaintenanceControl, RepairControl, RuntimeComponent,
    RuntimeControl, RuntimePortError, RuntimeReaders, RuntimeWriter, ShardRuntimeAttachment,
    ShardRuntimeParts,
};

#[cfg(test)]
mod tests;
