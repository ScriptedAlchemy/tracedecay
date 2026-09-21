//! Consolidated SDK integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `remote_client_proxy` stays its own binary because it
//! mutates the process-wide proxy environment.
//!
//! The sdk-conformance `production-router` job runs this whole binary with
//! `--run-ignored all`, so a `#[ignore]`d daemon test here is covered by CI
//! without any further wiring.

#[path = "../../../../tests/support/isolated_profile.rs"]
mod isolated_profile;

mod client;
mod facade;
mod production_daemon;
mod semantic_replay;

fn production_binary() -> std::path::PathBuf {
    let path = std::env::var_os("TRACEDECAY_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("../../target/debug/tracedecay"));
    std::fs::canonicalize(&path)
        .unwrap_or_else(|error| panic!("missing production daemon {}: {error}", path.display()))
}

fn run(command: &mut std::process::Command) -> Vec<u8> {
    isolated_profile::run_ok(command, "command")
}
