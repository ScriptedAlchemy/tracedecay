//! Atomic receipt, checkpoint, idempotency and effect bookkeeping for native Turso.
//! The writer owns transaction control; ledger records share its request savepoint.
mod checkpoint;
mod commit;
mod error;
mod idempotency;
mod inbox;
mod outbox;
mod prune;
mod schema;
mod sql;

pub(crate) use checkpoint::current_watermark;
pub(crate) use commit::record_runtime_commit;
pub use error::LedgerError;
pub(crate) use idempotency::{LedgerDisposition, lookup_receipt};
pub use schema::RUNTIME_LEDGER_SCHEMA;
pub(crate) use schema::initialize_writer_ledger;
pub use sql::RequestSql;
pub(crate) use sql::{Row, parameter, params};

#[cfg(test)]
mod tests;

pub(crate) fn disposition(
    transaction: &RequestSql<'_>,
    request: &tracedecay_store::RuntimeSubmitRequestV1,
) -> Result<LedgerDisposition, LedgerError> {
    let submission =
        sql::Submission::new(&request.envelope().metadata, request.transaction_scope())?;
    idempotency::disposition(transaction, &submission)
}
