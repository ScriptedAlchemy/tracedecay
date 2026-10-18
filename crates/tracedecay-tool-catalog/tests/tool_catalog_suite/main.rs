//! Consolidated tool-catalog integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `common` is compiled once here and reached through
//! `use crate::common;` from the modules that used to declare it.

mod common;

mod executable_binding_contract;
mod manifest_contract;
mod profile_budget;
mod retrieval_contract;
mod snapshot_contract;
