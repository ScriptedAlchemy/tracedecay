//! Transaction-bound receipt, checkpoint, idempotency, and outbox bookkeeping.
//!
//! The writer supplies the transaction capability. The ledger never opens or
//! commits a connection, so its records share the domain mutation's boundary.

mod checkpoint;
mod commit;
mod error;
mod idempotency;
mod inbox;
mod outbox;
mod prune;
mod schema;
mod sqlite;

#[cfg(test)]
pub(crate) use checkpoint::current_watermark;
#[cfg(test)]
pub(crate) use commit::record_commit;
pub(crate) use commit::record_runtime_commit;
pub(crate) use error::LedgerError;
pub(crate) use idempotency::{LedgerDisposition, lookup_receipt};
#[cfg(test)]
pub(crate) use inbox::lookup as lookup_inbox;
#[cfg(test)]
pub(crate) use outbox::outbox_entry;
pub use schema::{
    COPY_RETIRED_IDEMPOTENCY_LEDGER_PAGE_SQL, DELETE_CONVERGED_IDEMPOTENCY_LEDGER_PAGE_SQL,
    DROP_RETIRED_IDEMPOTENCY_LEDGER_SQL, RETIRED_IDEMPOTENCY_LEDGER_PRESENT_SQL,
    RUNTIME_LEDGER_SCHEMA,
};
pub(crate) use schema::{initialize_schema, retired_idempotency_ledger_present};

#[cfg(test)]
mod tests;
