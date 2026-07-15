//! Consolidated memory test suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the shard wall clock. These modules used to be the standalone
//! `memory_test` and `memory_eval_test` binaries; merging them into one
//! binary removes a link step while keeping every test (names gain a module
//! prefix, e.g. `memory_test::...`, `memory_eval_test::...`).

#[path = "../common/mod.rs"]
mod common;

mod memory_eval_test;
mod memory_test;
