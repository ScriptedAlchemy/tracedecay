//! Bundled SQLite storage runtime.

mod admission;
pub mod backup;
mod checkpoint;
mod connection;
pub use connection::{ConnectionPolicyError, open_immutable_health_reader, open_immutable_reader};
pub mod effects;
pub mod evidence;
pub mod graph;
mod ledger;
pub mod maintenance;
mod migration;
mod operation;
mod persistence;
pub mod read_consistency;
pub mod reader;
pub mod repair;
pub mod repository;
pub mod runtime;
#[cfg(test)]
mod s11_evidence_tests;
mod telemetry;
#[cfg(test)]
mod test_support;
pub mod watermark;
mod writer;

pub use checkpoint::{
    CheckpointBlocker, CheckpointBlockers, CheckpointFrameReport, CheckpointInterruption,
    CheckpointKind, CheckpointOutcome, CheckpointPressure, CheckpointStatus, CheckpointWal,
    MaintenanceCheckpointMode,
};
pub use operation::StorageOperationExecutor;
pub use telemetry::{
    WriterBatchMetrics, WriterBatchTotals, WriterClientServiceSnapshot, WriterCommitSnapshot,
    WriterOperationCounters, WriterQueueSnapshot, WriterServiceCounts, WriterTelemetrySnapshot,
};
pub use writer::{
    CheckpointControlError, CheckpointHandle, CheckpointRequest, CheckpointTicket,
    ExistingWriterLocator, MaintenanceCheckpointRequest, PersistentWriter, WriterActorError,
    WriterStartError, WriterState,
};
