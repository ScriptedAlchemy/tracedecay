//! Consolidated automation test suite.
//!
//! Merges the former automation_backend_test, automation_config_test,
//! automation_memory_curator_runner_test, automation_run_ledger_test,
//! automation_runner_test, automation_scheduler_test,
//! automation_session_reflector_runner_test, and
//! automation_skill_writer_runner_test binaries into one integration-test
//! binary so Windows CI links one executable instead of eight. The binary
//! keeps the automation_runner_test name because automation artifacts embed
//! `cargo test --test automation_runner_test ...` replay commands.

// Full-journey Hotpath builds compose several measured automation futures in
// each test body; keep the expanded query budget local to this test crate.
#![recursion_limit = "256"]

#[path = "../common/mod.rs"]
mod common;

mod support;

mod backend;
mod combined_review;
mod config;
mod jobs;
mod memory_curator;
mod run_ledger;
mod runner;
mod scheduler;
mod session_reflector;
mod skill_writer;
mod skill_writer_consolidation;
