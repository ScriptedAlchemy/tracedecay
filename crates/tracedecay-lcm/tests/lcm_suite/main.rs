//! Consolidated LCM integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix.

mod compression_policy;
mod lcm_contracts;
mod lcm_security;
