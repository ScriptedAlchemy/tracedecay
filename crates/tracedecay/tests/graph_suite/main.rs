//! Consolidated in-process test suite for graph, types, display, context,
//! resolution, cloud, annotation-helper, and complexity tests.
//!
//! Merging these formerly separate integration-test binaries into one binary
//! cuts Windows CI link time (each `tests/*.rs` file links separately).

#![allow(clippy::too_many_lines)]
#[path = "../common/mod.rs"]
mod common;

mod annotation_helpers_test;
mod complexity_test;
mod graph_test;
mod types_test;
