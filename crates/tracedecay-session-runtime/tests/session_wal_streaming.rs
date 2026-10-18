//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use support::{
    BASE_SESSIONS, DrainFixture, MESSAGES_PER_SESSION, PROMPT_TITLE_WORDS, PROVIDER,
    SEED_TIMESTAMP, capture, drain, message_requests, refresh_until_idle, seed_sessions,
    session_cursor, session_store_path, wal_checkpoint_sequence,
};
use tracedecay_session_runtime::session_sync::test_harness::SessionTemporalRefreshWakeState;
use tracedecay_sessions::admission::{HostAdmission, HostAdmissionScope};
use tracedecay_store::WAL_SOFT_LIMIT_BYTES;

/// Ingest keeps writing while readers keep reading, as in a live daemon. The
/// writer checkpoints once the log crosses the soft limit, and the log is
/// reused from its head only once a checkpoint returned every frame and no
/// reader still needs the old ones. A reader that pins the log, or a
/// checkpoint that never completes, leaves the writer appending without
/// bound; a restarted log stays at its retained size, the soft limit, plus
/// at most the one batch that crossed it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_stream_with_concurrent_readers_keeps_the_wal_bounded() {
    const STREAMED_SESSIONS: u64 = 2 * BASE_SESSIONS;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    let wal = session_store_path(&fixture).with_extension("db-wal");
    let streaming = AtomicBool::new(true);
    let reads = AtomicU64::new(0);
    let probe = format!("{}.message.00000", session_cursor(0).session_id.as_str());
    seed_sessions(&facade, project, &scope, 0..1).await;
    let restarts_before = wal_checkpoint_sequence(&wal);

    let stream = async {
        let mut largest = 0;
        for index in 1..STREAMED_SESSIONS {
            let mut cursor = session_cursor(index);
            let timestamp = SEED_TIMESTAMP + i64::try_from(index * MESSAGES_PER_SESSION).unwrap();
            capture(
                &facade,
                message_requests(
                    project,
                    &scope,
                    &mut cursor,
                    MESSAGES_PER_SESSION,
                    timestamp,
                    PROMPT_TITLE_WORDS,
                ),
            )
            .await;
            drain(&facade, &scope).await;
            refresh_until_idle(&database, &refresh).await;
            largest = largest.max(std::fs::metadata(&wal).unwrap().len());
        }
        streaming.store(false, Ordering::Release);
        largest
    };
    let reader = || async {
        while streaming.load(Ordering::Acquire) {
            assert!(
                facade
                    .has_session_message(&scope, PROVIDER, &probe)
                    .await
                    .unwrap()
            );
            reads.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
        }
    };
    let (largest_wal, (), ()) = tokio::join!(stream, reader(), reader());
    let restarts = wal_checkpoint_sequence(&wal) - restarts_before;

    eprintln!(
        "sustained stream: largest WAL {largest_wal} bytes, {restarts} log restarts, {} \
         concurrent reads",
        reads.load(Ordering::Relaxed)
    );
    assert!(reads.load(Ordering::Relaxed) > 0);
    assert!(
        largest_wal <= 2 * WAL_SOFT_LIMIT_BYTES,
        "the WAL must stay within twice its soft limit under a sustained stream with \
         concurrent readers: largest={largest_wal}"
    );
    assert!(
        restarts >= 4,
        "the stream must cross the soft limit and restart the log repeatedly: {restarts}"
    );
}
