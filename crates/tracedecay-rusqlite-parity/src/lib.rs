//! Process-isolated implementation of the SQLite parity protocol.
//!
//! This crate intentionally owns the bundled SQLite link boundary. Its DTOs
//! live in `tracedecay-sqlite-parity-protocol` so daemon-side orchestration can
//! use the exact same serde shapes without importing this crate.

mod closed_sql;
mod fixture_ddl;
mod service;
mod session;
mod snapshot;
mod sqlite_metadata;
mod transport;

#[cfg(test)]
mod tests;

pub use fixture_ddl::SESSION_STORE_FIXTURE_TABLES_DDL;
pub use transport::serve;
