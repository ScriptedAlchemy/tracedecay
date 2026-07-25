//! Canonical persistence for daemon-owned Git index transactions.
//!
//! This module owns the `SQLite` adapter only.  The DTOs and synchronous port
//! contract remain in `tracedecay-store`; daemon code bridges that contract to
//! this async adapter through its bounded mutation actor.

mod read;
mod schema;
mod store;

pub(crate) use read::GitIndexReadExecutor;
pub(crate) use schema::ensure_git_index_transaction_schema;
pub(crate) use store::GlobalDbGitIndexTransactionStore;

#[cfg(test)]
mod tests;
