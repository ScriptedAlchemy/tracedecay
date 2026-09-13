//! Consolidated rusqlite-runtime integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. The shared `common`, `work_registered_store`, and
//! `registered_workflow_store` harnesses are compiled once here and reached
//! through `use crate::<harness>;` from the modules that used to declare
//! them. `tests/hotpath_coverage.rs` stays its own binary because it sets
//! process environment variables (`HOTPATH_*`) in-process.

mod common;
mod registered_workflow_store;
// Shared with the root crate's storage_runtime_rusqlite_suite target; mounted once
// here so runtime_reader_restart and runtime_storage do not load the same file twice.
#[path = "../../../../tests/storage_runtime_rusqlite_suite/runtime_test_support.rs"]
mod runtime_test_support;
mod work_registered_store;

mod handoff_open_storage;
mod multi_root_scope_set;
mod repository_attachment;
mod runtime_actor;
mod runtime_reader_restart;
mod runtime_storage;
mod transactional_inbox;
mod work_attempt_storage;
mod work_duplicate_adjudication_storage;
mod work_leak_adjudication_storage;
mod work_placement_storage;
mod work_product_graph_authority;
mod work_product_query_authority;
mod work_run_control_storage;
mod work_storage;
mod workflow_fan_out_census_storage;
mod workflow_run_journal_storage;
mod workflow_runtime_storage;
