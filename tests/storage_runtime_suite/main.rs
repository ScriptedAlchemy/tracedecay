//! End-to-end storage-runtime integration tests that exercise the canonical
//! engine alongside the process-isolated SQLite parity helper.
//!
//! Session cases use production APIs and compare against the helper subprocess.

mod support;

mod session;
