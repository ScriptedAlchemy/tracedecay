//! Consolidated daemon test suite.
//!
//! Covers daemon fixture authority, PR-ref autotracking, and workflow handoff.

#[path = "../common/mod.rs"]
mod common;

mod fixture_authority_test;
#[cfg(unix)]
mod pr_autotrack_test;
mod workflow_handoff_test;
