//! In-process rusqlite storage-runtime cutover coverage (S5–S10).
//!
//! These cases open bundled/private rusqlite connections in-process. They must
//! not share a test binary with the libsql-backed graph/session parity suite:
//! libsql and rusqlite configure incompatible SQLite threading modes, and the
//! first initializer poisons the process-wide singleton for the other.

mod cutover_support;

mod s10_serialization;
mod s5_reader;
mod s6_operations;
mod s7_graph_attachment;
mod s8_repository_parity;
mod s9_effect_restart;
