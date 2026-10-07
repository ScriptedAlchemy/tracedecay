//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use support::{DrainFixture, project_history_pass, session_store_path, wal_commits, wal_mark};
use tracedecay_sessions::admission::HostAdmissionScope;

/// The daemon reruns the history pass every idle minute whether or not a
/// host wrote anything. A pass that finds nothing new must leave the store
/// as it was: every commit it makes is WAL every reader and checkpoint then
/// rereads, for as long as the daemon idles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_history_pass_commits_nothing() {
    let fixture = DrainFixture::open().await;
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let store = session_store_path(&fixture);
    let home = fixture._tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let start = wal_mark(&store);
    let first = project_history_pass(&fixture, &database, &home).await;
    let settled = wal_mark(&store);
    let idle = project_history_pass(&fixture, &database, &home).await;
    let idled = wal_mark(&store);

    assert!(
        first.failures.is_empty() && idle.failures.is_empty(),
        "history passes must not fail: first={:?} idle={:?}",
        first.failures,
        idle.failures,
    );
    assert!(
        wal_commits(&store, start, settled).is_some_and(|commits| commits > 0),
        "the first pass records each provider's coverage"
    );
    assert_eq!(
        wal_commits(&store, settled, idled),
        Some(0),
        "a history pass that finds nothing new must commit nothing"
    );
}
