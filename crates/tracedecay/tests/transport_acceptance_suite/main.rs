//! Consolidated transport-acceptance integration suite.
//!
//! Former standalone `test-transport` targets, compiled as modules of one
//! binary so a root-crate edit no longer relinks six ~700 MiB test binaries.

// Deeply nested async fixture bodies exceed rustc's default layout query
// depth under the perf profile; match the workspace-standard limit used by
// the tracedecay lib and CLI crate roots.
#![recursion_limit = "256"]

#[path = "../common/mod.rs"]
mod common;

mod daemon_fault_harness_test;
#[cfg(all(unix, feature = "test-transport"))]
mod git_index_snapshot_root_canonicalization;
#[cfg(feature = "test-transport")]
mod graph_rebuild_status_test;
#[cfg(unix)]
mod serve_proxy_lifecycle_test;
mod typed_terminal_restart_acceptance;
mod v2_surface_mount_conformance;
