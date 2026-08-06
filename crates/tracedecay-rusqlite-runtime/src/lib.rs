//! Bundled SQLite storage runtime.

mod admission;
mod authority;
pub mod backup;
mod checkpoint;
mod connection;
pub use connection::file_family::{
    SqliteFamilyComponent, SqliteFamilyIntegrityError, SqliteFamilyViolation,
};
pub use connection::{
    ConnectionPolicyError, OpenedDatabaseFileError, SqliteCatalogObject, SqliteSchemaInspection,
    SqliteSchemaInspectionError, inspect_existing_schema, open_immutable_health_reader,
    open_immutable_reader,
};
mod content_digest;
pub use content_digest::{CanonicalContentDigestError, canonical_session_domain_content_sha256};
#[doc(hidden)]
pub mod exact_sql;
pub mod graph;
mod ledger;
pub mod maintenance;
mod operation;
mod persistence;
pub mod read_consistency;
pub mod reader;
pub mod remote;
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

pub(crate) fn finalize_guarded_submit_outcome(
    outcome: tracedecay_store::RuntimeSubmitOutcomeV1,
    family_guard: &connection::file_family::SqliteFamilyGuard,
) -> Result<tracedecay_store::RuntimeSubmitOutcomeV1, SqliteFamilyIntegrityError> {
    if matches!(
        outcome,
        tracedecay_store::RuntimeSubmitOutcomeV1::CommitRecoveryRequired { .. }
    ) {
        return Ok(outcome);
    }
    match family_guard.probe() {
        Ok(()) => Ok(outcome),
        Err(_)
            if matches!(
                outcome,
                tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
                    | tracedecay_store::RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
            ) =>
        {
            Ok(
                tracedecay_store::RuntimeSubmitOutcomeV1::CommitRecoveryRequired {
                    reason: tracedecay_store::RuntimeCommitRecoveryReasonV1::PhysicalStoreIdentityChanged,
                },
            )
        }
        Err(error) => Err(error),
    }
}
