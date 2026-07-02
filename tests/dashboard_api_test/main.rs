//! Consolidated dashboard API integration tests.
//!
//! All `dashboard_*` integration tests live in this single binary so Windows
//! CI links one test executable instead of twelve. The binary keeps the
//! `dashboard_api_test` name because CI invokes it directly via
//! `cargo nextest run --test dashboard_api_test`.

#[path = "../common/mod.rs"]
mod common;

mod dashboard_api_support;

mod analytics;
mod api;
mod automation;
mod automation_config;
mod automation_skills;
mod code_diagnostics;
mod graph;
mod lcm;
mod lcm_fixes;
mod memory_curation;
mod projects;
mod savings;
