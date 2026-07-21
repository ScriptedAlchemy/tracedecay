//! Process-isolated implementation of the SQLite parity protocol.
//!
//! This crate intentionally owns the bundled SQLite link boundary. Its DTOs
//! live in `tracedecay-sqlite-parity-protocol` so daemon-side orchestration can
//! use the exact same serde shapes without importing this crate.

mod service;

pub use service::serve;
