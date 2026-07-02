//! Consolidated test suite for hook evaluation, hook branch routing, LSP
//! diagnostics, and extract-worker hardening tests.
//!
//! These tests spawn subprocesses (fake LSP servers, git, the tracedecay
//! binary) or mutate process-wide environment variables, so they live in a
//! separate binary from the pure in-process `graph_suite`. Merging the
//! formerly separate binaries cuts Windows CI link time.
//!
//! Env-mutating tests across all modules must serialize on
//! `common::GLOBAL_DB_ENV_LOCK` because they now share one process.

#[path = "../common/mod.rs"]
mod common;

mod extract_worker_test;
mod hook_branch_routing_test;
mod hooks_test;
mod lsp_code_diagnostics_test;
