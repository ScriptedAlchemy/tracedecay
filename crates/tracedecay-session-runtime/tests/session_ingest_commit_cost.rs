//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::sync::Arc;
use support::{
    BASE_SESSIONS, DrainFixture, PROMPT_TITLE_WORDS, SEED_TIMESTAMP, capture, drain,
    message_requests, refresh_until_idle, seed_sessions, session_store_path, wal_commits, wal_mark,
};
use tracedecay_session_runtime::session_sync::test_harness::SessionTemporalRefreshWakeState;
use tracedecay_sessions::admission::HostAdmissionScope;

/// One streamed message commits once per durability boundary.
///
/// Capture commits source presence, the observation, its external-source
/// receipt, and the message's Git evidence span before the host is
/// acknowledged. The drain commits the external-source replay, the
/// observation projection, and the Git evidence convergence. The temporal
/// refresh commits its projected batch: the operation's begin folds into
/// that same commit, the pending relation receipt the native graph write
/// recovers from, and the activation that settles that receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_commits_once_per_durability_boundary() {
    const MESSAGES: u64 = 8;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    let store = session_store_path(&fixture);

    let mut live = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    refresh_until_idle(&database, &refresh).await;
    let mut measured = Vec::new();
    for _ in 0..MESSAGES {
        let timestamp = SEED_TIMESTAMP + i64::try_from(live.next_ordinal).unwrap();
        let start = wal_mark(&store);
        capture(
            &facade,
            message_requests(project, &scope, &mut live, 1, timestamp, PROMPT_TITLE_WORDS),
        )
        .await;
        let captured = wal_mark(&store);
        assert_eq!(drain(&facade, &scope).await, 1);
        let drained = wal_mark(&store);
        assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
        let refreshed = wal_mark(&store);
        if let (Some(capture), Some(drain), Some(refresh)) = (
            wal_commits(&store, start, captured),
            wal_commits(&store, captured, drained),
            wal_commits(&store, drained, refreshed),
        ) {
            measured.push((capture, drain, refresh));
        }
    }

    eprintln!("streamed message commits (capture, drain, refresh): {measured:?}");
    assert!(
        measured.len() >= 4,
        "most messages must land within one log generation: {measured:?}"
    );
    assert!(
        measured.iter().all(|commits| *commits == (4, 3, 3)),
        "one streamed message must commit once per durability boundary: {measured:?}"
    );
}
