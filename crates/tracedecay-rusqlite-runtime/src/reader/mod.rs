//! Bounded, snapshot-coherent readers for one already-authorized SQLite shard.
//!
//! The pool accepts only an explicit verified locator. Every SQLite open occurs
//! on a dedicated worker thread, and every query is selected by the closed
//! [`RuntimeReadRequestV1`] contract rather than caller-provided SQL.

mod locator;
mod pool;
mod worker;

pub use locator::{ExistingReaderLocator, ReaderStartError};
pub use pool::{
    ReaderAcquireError, ReaderLease, ReaderPool, ReaderPoolSnapshot, ReaderPoolState, SnapshotLease,
};
pub use worker::{
    ExactSqlOnlyReaderV1, ReaderQueryExecutor, ReaderWorkerError, StoreSizeTelemetrySample,
    TableSizeTelemetrySample,
};

#[cfg(test)]
mod tests;

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
