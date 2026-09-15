//! Consolidated `test-helpers` contract suite for the graph database.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs` gated on the same `test-helpers` feature; every one
//! of them linked the same dependency closure. Compiled as modules of one
//! binary, each test keeps its old binary name as its module prefix. The
//! shared `tests/support` harness stays where the standalone measurement
//! probes can still reach it and is compiled once here; the modules that used
//! to declare `mod support;` now `use crate::support;`.
//!
//! The at-rest, RSS, and replay probes stay separate binaries: each measures
//! process-wide state (`VmHWM`, a counting global allocator) and documents
//! that it must run one scenario per process.

#[path = "../support/mod.rs"]
mod support;

mod backup_contract;
mod cross_projection_contract;
mod durability_crash_contract;
mod open_error_contract;
mod paged_relation_ids;
mod point_read_contract;
mod projection_read_contract;
mod recovered_generation_digest_contract;
mod registry_contract;
mod runtime_contract;
mod verified_generation_contract;
