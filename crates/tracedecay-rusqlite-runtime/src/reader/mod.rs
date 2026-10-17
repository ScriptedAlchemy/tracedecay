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

/// How long a read snapshot may stay open. Past it the worker rolls the read
/// back and the snapshot's later reads fail as expired, so no reader pins the
/// WAL indefinitely. One full statement budget fits inside it.
pub const READ_SNAPSHOT_LIFETIME: std::time::Duration = std::time::Duration::from_secs(30);

/// The lifetime of a schema-convergence audit snapshot, whose single
/// statements scan a whole table and so carry no per-statement deadline.
pub const CONVERGENCE_SNAPSHOT_LIFETIME: std::time::Duration = std::time::Duration::from_hours(1);

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
