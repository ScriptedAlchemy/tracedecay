//! Consolidated SDK integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix (the sdk-conformance workflow selects the
//! `production_daemon::` module by that prefix). `remote_client_proxy` stays
//! its own binary because it mutates the process-wide proxy environment.

mod client;
mod facade;
mod production_daemon;
mod semantic_replay;
