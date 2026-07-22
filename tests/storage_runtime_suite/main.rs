//! End-to-end storage-runtime integration tests that exercise libsql alongside
//! the process-isolated rusqlite parity helper.
//!
//! Graph and session cases stay in this binary so they can open libsql through
//! production APIs and compare against the helper subprocess. In-process
//! rusqlite cutover coverage lives in `storage_runtime_rusqlite_suite` so the
//! two SQLite threading-mode initializers never share a process.

mod support;

mod graph;
mod session;
