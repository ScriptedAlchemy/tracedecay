//! Consolidated `test-transport` host-journey acceptance suite.
//!
//! Both modules were standalone integration-test binaries gated on the same
//! `test-transport` feature, and each one linked the complete CLI dependency
//! closure. Compiled as modules of one binary, each test keeps its old binary
//! name as its module prefix. `work_loop_journey` stays its own binary because
//! it mutates process environment variables in-process.

mod host_lifecycle_cli_acceptance;
mod opencode_one_analyzer_journey;
