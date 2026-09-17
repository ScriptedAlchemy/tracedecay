//! Consolidated memory test suite.
//!
//! Windows CI links every integration-test binary separately, so retained
//! memory coverage stays in one binary to avoid an extra link step.

#![allow(clippy::too_many_lines)]
#[path = "../common/mod.rs"]
mod common;

mod memory_eval_test;
