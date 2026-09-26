//! Consolidated test suite for hook evaluation, hook branch routing, and LSP
//! diagnostics tests.
//!
//! These tests spawn subprocesses (fake LSP servers, git, the tracedecay
//! binary), so they live in a separate binary from the pure in-process
//! `graph_suite`. Merging the formerly separate binaries cuts Windows CI link
//! time. Every fixture hands its profile explicitly; none mutates the process
//! environment.

#![allow(clippy::too_many_lines)]
#[path = "../common/mod.rs"]
mod common;

#[cfg(feature = "test-transport")]
mod hint_settlement_test;
#[cfg(feature = "test-transport")]
mod hook_branch_routing_test;
mod hook_lifecycle_lease_test;
mod hook_replay_test;
mod hooks_test;
mod lsp_gateway_protocol_test;
