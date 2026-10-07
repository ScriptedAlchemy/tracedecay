//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use support::{
    BASE_SESSIONS, DrainFixture, GROWN_SESSIONS, MESSAGES_PER_SESSION, SEED_TIMESTAMP,
    probe_drain_after_wal_restart, seed_sessions, session_cursor,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_message_drain_reads_do_not_scale_with_the_session_store() {
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let mut probed = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;
    let mut padding = session_cursor(GROWN_SESSIONS);
    let base_read = probe_drain_after_wal_restart(
        &facade,
        &fixture,
        &mut probed,
        &mut padding,
        probe_timestamp,
    )
    .await;

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    assert_eq!(
        fixture
            .runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(GROWN_SESSIONS * MESSAGES_PER_SESSION + 1 + padding.next_ordinal).unwrap(),
    );
    let grown_read = probe_drain_after_wal_restart(
        &facade,
        &fixture,
        &mut probed,
        &mut padding,
        probe_timestamp + 1,
    )
    .await;

    assert_eq!(
        fixture
            .runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(GROWN_SESSIONS * MESSAGES_PER_SESSION + 2 + padding.next_ordinal).unwrap(),
    );

    eprintln!("single-message drain read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read <= base_read * 2,
        "an 8x larger session store must not double one message's drain reads: \
         base={base_read} grown={grown_read}"
    );
}
