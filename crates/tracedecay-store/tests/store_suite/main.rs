//! Consolidated store-crate integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix.

mod configuration_contract;
mod diagnostics_contract;
mod external_source_commit;
mod multi_root_cas_contract;
mod session_contract;
mod storage_runtime_contract;
