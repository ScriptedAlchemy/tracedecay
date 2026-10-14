//! Bundled SQLite storage runtime.

mod admission;
mod authority;
mod checkpoint;
mod connection;
pub mod schema_version;
pub use connection::{
    ConnectionPolicyError, OpenedDatabaseFileError, VerifiedReader, VerifiedReaderError,
    open_immutable_reader, open_verified_immutable_reader, open_verified_read_snapshot,
};
#[doc(hidden)]
pub mod exact_sql;
pub mod handoff;
mod ledger;
/// Canonical schema for the runtime writer ledger installed in registered
/// SQLite stores.
pub mod runtime_ledger {
    pub use crate::ledger::{RETIRE_RUNTIME_EFFECTS_V1, RUNTIME_LEDGER_SCHEMA};
}
pub mod native_runtime;
mod operation;
mod persistence;
pub mod reader;
pub mod remote;
pub mod repository;
pub mod runtime;
mod telemetry;
#[cfg(test)]
mod test_support;
pub mod watermark;
pub mod work;
pub mod work_attempt;
pub mod work_placement;
pub mod work_product;
pub mod work_run_control;
pub mod workflow;
mod writer;

pub use authority::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage,
};
pub use checkpoint::{
    CheckpointBlocker, CheckpointBlockers, CheckpointFrameReport, CheckpointInterruption,
    CheckpointKind, CheckpointOutcome, CheckpointPressure, CheckpointStatus, CheckpointWal,
};
pub use operation::{StorageOperationError, StorageOperationExecutor};
pub use telemetry::{
    ReaderAdmissionSnapshot, SqliteStoreSizeTelemetryPort, SqliteVmSnapshot, WalCheckpointSample,
    WalCheckpointSnapshot, WriterBatchMetrics, WriterBatchTotals, WriterClientServiceSnapshot,
    WriterCommitSnapshot, WriterLockWorkSnapshot, WriterOperationCounters, WriterQueueSnapshot,
    WriterServiceCounts, WriterTelemetrySnapshot, WriterTransactionMetrics,
    WriterTransactionOutcome, WriterTransactionTotals,
};
pub use writer::{
    CheckpointControlError, CheckpointHandle, CheckpointRequest, CheckpointTicket,
    ExistingWriterLocator, PersistentWriter, WriterActorError, WriterStartError, WriterState,
};
