//! Consolidated dashboard API integration tests.
//!
//! All `dashboard_*` integration tests live in this single binary so Windows
//! CI links one test executable instead of twelve. The binary keeps the
//! `dashboard_api_test` name because CI invokes it directly via
//! `cargo nextest run --test dashboard_api_test`.

mod common;
mod runtime;

mod dashboard_api_support;

mod analytics;
mod api;
mod assets;
mod automation;
mod automation_config;
mod automation_jobs;
mod automation_skills;
mod code_diagnostics;
mod delivery;
mod doctor;
mod explorer;
mod graph;
mod lcm;
mod loom;
mod memory_curation;
mod projects;
mod savings;
mod settings;
mod storage;
