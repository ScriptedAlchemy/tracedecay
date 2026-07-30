//! Consolidated session/LCM sqlite test suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the shard wall clock. These modules used to be nine standalone
//! `tests/session_*` binaries; merging them into one binary removes eight
//! link steps while keeping every test (names gain a module prefix, e.g.
//! `lcm_compression::...`).

#[path = "../common/mod.rs"]
mod common;

mod anchor_resolution;
mod anchor_tombstone_expiry;
mod fact_anchor_authority;
mod git_backfill;
mod global_db;
mod lcm_compression;
mod lcm_dag;
mod lcm_payload;
mod lcm_query;
mod lcm_raw;
mod lcm_summary_lineage_review;
mod message_search_eval_test;
mod observation_application;
mod observation_projection;
mod observation_store;
mod observation_workflow_projection;
mod structured_backfill;
mod temporal_application;
mod temporal_benchmark;
mod temporal_derived_evidence;
mod temporal_projection;
mod temporal_refresh;
mod temporal_refresh_application;
mod transcript_backfill;
mod transcript_store;
