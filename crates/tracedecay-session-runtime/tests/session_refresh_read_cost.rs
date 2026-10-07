//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::sync::Arc;
use support::{
    BASE_SESSIONS, DrainFixture, GROWN_SESSIONS, SEED_TIMESTAMP, probe_one_message,
    process_read_bytes, refresh_until_idle, seed_sessions,
};
use tracedecay_session_runtime::session_sync::test_harness::SessionTemporalRefreshWakeState;
use tracedecay_sessions::admission::HostAdmissionScope;

/// Each probe starts from emptied connection caches, so the comparison counts
/// the pages one message touches rather than how much of each store the
/// caches still hold after seeding: a cache that holds the whole base store
/// but only part of the grown one would otherwise read as growth.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_refresh_reads_do_not_scale_with_the_session_store() {
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let mut probed = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    assert_eq!(
        refresh_until_idle(&database, &refresh).await,
        usize::try_from(BASE_SESSIONS).unwrap()
    );
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;
    database.release_connection_memory().await.unwrap();
    let before = process_read_bytes();
    probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp).await;
    assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
    let base_read = process_read_bytes() - before;

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    assert_eq!(
        refresh_until_idle(&database, &refresh).await,
        usize::try_from(GROWN_SESSIONS - BASE_SESSIONS).unwrap()
    );
    database.release_connection_memory().await.unwrap();
    let before = process_read_bytes();
    probe_one_message(&facade, project, &scope, &mut probed, probe_timestamp + 1).await;
    assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
    let grown_read = process_read_bytes() - before;

    eprintln!("streamed message ingest read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read <= base_read * 2,
        "an 8x larger session store must not double one streamed message's reads: \
         base={base_read} grown={grown_read}"
    );
}
