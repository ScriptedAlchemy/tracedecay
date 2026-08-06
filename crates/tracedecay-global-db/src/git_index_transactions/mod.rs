//! Canonical persistence for daemon-owned Git index transactions.
//!
//! This module owns the `SQLite` adapter only.  The DTOs and synchronous port
//! contract remain in `tracedecay-store`; daemon code bridges that contract to
//! this async adapter through its bounded mutation actor.

mod database;
mod read;
mod schema;
mod store;

pub use read::GitIndexReadExecutor;
pub use schema::ensure_git_index_transaction_schema;
pub use store::{GitIndexPreviewInputGcResult, GlobalDbGitIndexTransactionStore};

#[cfg(test)]
mod tests;
