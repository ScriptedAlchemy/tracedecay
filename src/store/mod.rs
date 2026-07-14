//! Root-crate persistence adapters for store-facing contracts.
//!
//! Adapters in this module borrow already-open authoritative stores. They do
//! not discover paths, open connections, or own transaction state.

pub mod global_db;

pub use global_db::GlobalDbTranscriptStore;
