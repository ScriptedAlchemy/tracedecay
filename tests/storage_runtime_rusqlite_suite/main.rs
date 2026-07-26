//! In-process SQLite storage-runtime cutover coverage (S5–S10).
//!
//! These cases exercise the bundled/private SQLite engine in-process and stay
//! separate from the subprocess parity suite so process-isolation behavior is
//! covered independently.

mod cutover_support;

mod s10_serialization;
mod s5_reader;
mod s6_operations;
mod s7_graph_attachment;
mod s8_repository_parity;
