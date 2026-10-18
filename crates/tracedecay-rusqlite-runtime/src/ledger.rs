//! Transaction-bound receipt, checkpoint, and idempotency bookkeeping.
//!
//! The writer supplies the transaction capability. The ledger never opens or
//! commits a connection, so its records share the domain mutation's boundary.

mod checkpoint;
mod commit;
mod error;
mod idempotency;
mod prune;
mod schema;
mod sqlite;

pub(crate) use checkpoint::current_watermark;
#[cfg(test)]
pub(crate) use commit::record_commit;
pub(crate) use commit::record_runtime_commit;
pub(crate) use error::LedgerError;
pub(crate) use idempotency::{LedgerDisposition, lookup_receipt};
pub(crate) use schema::initialize_schema;
pub use schema::{RETIRE_RUNTIME_EFFECTS_V1, RUNTIME_LEDGER_SCHEMA};

#[cfg(test)]
mod tests;
