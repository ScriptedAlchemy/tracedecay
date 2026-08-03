//! Bundled SQLite storage runtime.

mod admission;
mod authority;
pub mod backup;
mod checkpoint;
mod connection;
pub use connection::{
    ConnectionPolicyError, OpenedDatabaseFileError, open_immutable_health_reader,
    open_immutable_reader,
};
mod content_digest;
pub use content_digest::{CanonicalContentDigestError, canonical_session_domain_content_sha256};
pub mod graph;
mod ledger;
pub mod maintenance;
#[doc(hidden)]
pub mod migration_sql;
mod operation;
mod persistence;
pub mod read_consistency;
pub mod reader;
pub mod repository;
pub mod runtime;
mod telemetry;
#[cfg(test)]
mod test_support;
pub mod watermark;
pub mod work;
pub mod workflow;
mod writer;

pub use authority::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage,
};
pub use checkpoint::{
    CheckpointBlocker, CheckpointBlockers, CheckpointFrameReport, CheckpointInterruption,
    CheckpointKind, CheckpointOutcome, CheckpointPressure, CheckpointStatus, CheckpointWal,
    MaintenanceCheckpointMode,
};
pub use operation::StorageOperationExecutor;
pub use telemetry::{
    SqliteStoreSizeTelemetryPort, WriterBatchMetrics, WriterBatchTotals,
    WriterClientServiceSnapshot, WriterCommitSnapshot, WriterOperationCounters,
    WriterQueueSnapshot, WriterServiceCounts, WriterTelemetrySnapshot,
};
pub use writer::{
    CheckpointControlError, CheckpointHandle, CheckpointRequest, CheckpointTicket,
    ExistingWriterLocator, MaintenanceCheckpointRequest, OnlineBackupReceipt, PersistentWriter,
    WriterActorError, WriterOnlineBackupError, WriterStartError, WriterState,
};
