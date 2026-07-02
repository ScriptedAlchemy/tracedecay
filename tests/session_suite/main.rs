//! Consolidated session/LCM sqlite test suite.
//!
//! Windows CI links every integration-test binary separately, and link time
//! dominates the shard wall clock. These modules used to be nine standalone
//! `tests/session_*` binaries; merging them into one binary removes eight
//! link steps while keeping every test (names gain a module prefix, e.g.
//! `lcm_compression::...`).

#[path = "../common/mod.rs"]
mod common;

mod global_db;
mod lcm_compression;
mod lcm_dag;
mod lcm_ingest_protection;
mod lcm_payload;
mod lcm_query;
mod lcm_raw;
mod lcm_schema;
mod transcript_backfill;
