//! Canonical persistence for daemon-owned Git index transactions.
//!
//! This module owns the SQLite adapter only.  The DTOs and synchronous port
//! contract remain in `tracedecay-store`; daemon code bridges that contract to
//! this async adapter through its bounded mutation actor.

mod schema;
mod store;

pub(super) use schema::ensure_git_index_transaction_schema;
pub(crate) use store::GlobalDbGitIndexTransactionStore;

#[cfg(test)]
mod tests;
