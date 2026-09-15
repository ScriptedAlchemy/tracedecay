//! Consolidated policy-crate integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. The `tests/fixtures` truth tables stay where the
//! contracts crate's shared harness also reads them.

mod curation_apply;
mod routing_admission;
mod sink_recheck;
mod source_authorization;
mod work_planner;
