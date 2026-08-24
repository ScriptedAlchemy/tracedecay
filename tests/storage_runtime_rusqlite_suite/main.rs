//! In-process SQLite storage-runtime coverage.
//!
//! These cases exercise the bundled/private SQLite engine in-process and stay
//! separate from the subprocess parity suite so process-isolation behavior is
//! covered independently.

mod runtime_test_support;

mod repository_parity;
mod runtime_operations;
mod runtime_reader;
mod writer_serialization;
