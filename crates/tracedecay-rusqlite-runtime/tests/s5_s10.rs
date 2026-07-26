//! Reachable SQLite storage-runtime acceptance target.

#[path = "../../../tests/storage_runtime_rusqlite_suite/cutover_support.rs"]
mod cutover_support;

#[path = "../../../tests/storage_runtime_rusqlite_suite/s10_serialization.rs"]
mod s10_serialization;
#[path = "../../../tests/storage_runtime_rusqlite_suite/s5_reader.rs"]
mod s5_reader;
#[path = "../../../tests/storage_runtime_rusqlite_suite/s6_operations.rs"]
mod s6_operations;
#[path = "../../../tests/storage_runtime_rusqlite_suite/s7_graph_attachment.rs"]
mod s7_graph_attachment;
#[path = "../../../tests/storage_runtime_rusqlite_suite/s8_repository_parity.rs"]
mod s8_repository_parity;
