//! Canonical persistence for daemon-owned native integration transactions.
//!
//! This module owns the `SQLite` adapter only. The DTOs and synchronous port
//! contract remain in `tracedecay-store`; daemon code bridges that contract to
//! this async adapter through its bounded mutation actor.

mod schema;
mod store;
mod worktree_cleanup;

pub use schema::ensure_native_integration_schema;
pub use store::GlobalDbNativeIntegrationStore;

#[cfg(test)]
mod tests;
