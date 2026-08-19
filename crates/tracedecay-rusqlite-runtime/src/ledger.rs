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
pub(crate) use schema::initialize_schema;

#[cfg(test)]
mod tests;
