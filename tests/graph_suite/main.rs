#![allow(clippy::collapsible_if)]
//! Consolidated in-process test suite for graph, types, display, context,
//! resolution, bench, cloud, annotation-helper, and complexity tests.
//!
//! Merging these formerly separate integration-test binaries into one binary
//! cuts Windows CI link time (each `tests/*.rs` file links separately).

#[path = "../common/mod.rs"]
mod common;

mod bench_test;
mod cloud_test;
mod complexity_test;
mod context_test;
mod display_test;
mod graph_test;
mod resolution_test;
mod types_test;
