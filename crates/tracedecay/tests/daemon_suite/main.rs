//! Consolidated daemon test suite.
//!
//! Covers the git-metadata watcher (design D3), the backstop scheduler (D5),
//! the concurrency governor, and branch-store GC (D6) — the freshness path the
//! daemon drives when git operations happen outside a hooked session.
//!
//! The `GitWatcher` type itself is a crate-private daemon component, so these
//! integration tests validate the *composed behavior* through the same public
//! APIs the watcher orchestrates (`TraceDecay::sync*`, `stale_files_since_commit`,
//! `add_branch_tracking_with_options`) against
//! real temp git repos. Watcher-internal wiring (debounce coalescing, event
//! classification, heartbeat staleness) is unit-tested inline in
//! `src/daemon/git_watch.rs`.

#![allow(clippy::too_many_lines)]
#[path = "../common/mod.rs"]
mod common;

mod advanced_workflow_journey_test;
#[cfg(unix)]
mod authentication_refusal_test;
mod code_index_ignored_dependencies_test;
#[cfg(unix)]
mod code_index_journey;
#[cfg(unix)]
mod dirty_worktree_symbol_reads_test;
#[cfg(feature = "test-transport")]
mod git_watch_test;
#[cfg(all(unix, feature = "test-transport"))]
mod indexing_lifecycle_test;
mod invocation_observability;
mod invocation_primitives;
#[cfg(unix)]
#[cfg(unix)]
mod stale_client_resilience_test;
mod workflow_handoff_test;

#[test]
fn missing_cli_binary_reports_fixture_error() {
    if std::env::var_os("TRACEDECAY_TEST_MISSING_CLI_CHILD").is_some() {
        common::tracedecay_bin();
        return;
    }

    let scratch = tempfile::tempdir().expect("missing CLI test directory");
    let missing_binary = scratch.path().join("tracedecay");
    let output =
        std::process::Command::new(std::env::current_exe().expect("daemon suite executable path"))
            .args([
                "--exact",
                "missing_cli_binary_reports_fixture_error",
                "--nocapture",
            ])
            .env("TRACEDECAY_TEST_MISSING_CLI_CHILD", "1")
            .env("TRACEDECAY_TEST_BIN", &missing_binary)
            .output()
            .expect("missing CLI child process");

    assert!(!output.status.success(), "missing CLI child succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("CLI binary not built:")),
        "missing CLI failure was not typed:\n{stderr}"
    );
}
