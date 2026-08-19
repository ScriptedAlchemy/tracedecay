//! Reachable SQLite storage-runtime acceptance target.

#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_test_support.rs"]
mod runtime_test_support;

#[path = "../../../tests/storage_runtime_rusqlite_suite/repository_parity.rs"]
mod repository_parity;
#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_operations.rs"]
mod runtime_operations;
#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_reader.rs"]
mod runtime_reader;
#[path = "../../../tests/storage_runtime_rusqlite_suite/writer_serialization.rs"]
mod writer_serialization;
