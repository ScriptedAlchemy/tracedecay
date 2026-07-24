//! End-to-end storage-runtime integration tests that exercise the canonical
//! engine alongside the process-isolated SQLite parity helper.
//!
//! Graph and session cases stay in this binary so they use production APIs and
//! compare against the helper subprocess. In-process cutover coverage lives in
//! `storage_runtime_rusqlite_suite`.

mod support;

mod graph;
mod session;
