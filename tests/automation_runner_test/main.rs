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

#[path = "../common/mod.rs"]
mod common;

mod support;

mod backend;
mod combined_review;
mod config;
mod memory_curator;
mod run_ledger;
mod runner;
mod scheduler;
mod session_reflector;
mod skill_writer;
