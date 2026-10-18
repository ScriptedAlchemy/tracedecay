//! Bounded, snapshot-coherent readers for one already-authorized SQLite shard.
//!
//! The pool accepts only an explicit verified locator. Every SQLite open occurs
//! in a worker task on the storage pool, and every query is selected by the closed
//! [`RuntimeReadRequestV1`] contract rather than caller-provided SQL.

mod locator;
mod pool;
mod worker;

pub use crate::telemetry::ReaderAdmissionSnapshot;
pub use locator::{ExistingReaderLocator, ReaderStartError};
pub(crate) use pool::ReaderCheckpointBlockers;
pub use pool::{
    ReaderAcquireError, ReaderLease, ReaderPool, ReaderPoolSnapshot, ReaderPoolState, SnapshotLease,
};
pub use worker::{
    ExactSqlOnlyReaderV1, ReaderQueryExecutor, ReaderWorkerError, StoreSizeTelemetrySample,
    TableSizeTelemetrySample,
};

#[cfg(test)]
mod tests;

/// How long a read snapshot may sit with no statement running. Past it the
/// worker rolls the read back and the snapshot's later reads fail as expired,
/// so a snapshot its owner forgot, or holds across unrelated work, does not
/// pin the WAL. A long read that keeps issuing statements is not idle.
pub const READ_SNAPSHOT_IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest any read snapshot stays open, busy or not. It covers a
/// multi-statement read of a whole multi-gigabyte store, such as a
/// schema-convergence audit; every statement also stops at it.
pub const READ_SNAPSHOT_LIFETIME: std::time::Duration = std::time::Duration::from_hours(1);

use tracedecay_store::{
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, StorageRuntimeErrorV1, UnavailableReasonV1,
};

fn unavailable_read(
    reason: UnavailableReasonV1,
) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
    RuntimeReadOutcomeV1::new(
        None,
        tracedecay_store::RuntimeReadCoverageV1::Unavailable {
            coverage: None,
            reason,
        },
    )
    .map_err(|_| StorageRuntimeErrorV1::Infrastructure {
        operation: "construct typed unavailable reader outcome".to_owned(),
    })
}

fn validate_outcome(
    request: &RuntimeReadRequestV1,
    outcome: RuntimeReadOutcomeV1,
) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
    outcome
        .validate_for(request)
        .map_err(|_| StorageRuntimeErrorV1::Infrastructure {
            operation: "validate typed reader outcome".to_owned(),
        })?;
    Ok(outcome)
}
