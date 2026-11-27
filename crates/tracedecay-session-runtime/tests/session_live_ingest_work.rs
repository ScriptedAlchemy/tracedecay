//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::sync::Arc;
use support::{
    DrainFixture, PROMPT_TITLE_WORDS, SEED_TIMESTAMP, capture, drain, ingest_live_messages,
    ingest_live_messages_after_wal_restart, message_requests, refresh_until_idle, session_cursor,
};
use tracedecay_session_runtime::session_sync::test_harness::SessionTemporalRefreshWakeState;
use tracedecay_sessions::admission::HostAdmissionScope;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_ingest_work_does_not_grow_with_the_live_session() {
    const BASE_LENGTH: u64 = 16;
    const PROBE_MESSAGES: u64 = 4;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let mut live = session_cursor(0);
    let mut padding = session_cursor(1);

    ingest_live_messages(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut live,
        BASE_LENGTH - PROBE_MESSAGES,
    )
    .await;
    let base_work = ingest_live_messages_after_wal_restart(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut live,
        &mut padding,
        PROBE_MESSAGES,
    )
    .await;
    assert_eq!(live.next_ordinal, BASE_LENGTH);

    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    capture(
        &facade,
        message_requests(
            project,
            &scope,
            &mut live,
            6 * BASE_LENGTH,
            SEED_TIMESTAMP + i64::try_from(BASE_LENGTH).unwrap(),
            PROMPT_TITLE_WORDS,
        ),
    )
    .await;
    drain(&facade, &scope).await;
    assert_eq!(refresh_until_idle(&database, &refresh).await, 1);
    ingest_live_messages(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut live,
        BASE_LENGTH - PROBE_MESSAGES,
    )
    .await;
    let grown_work = ingest_live_messages_after_wal_restart(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut live,
        &mut padding,
        PROBE_MESSAGES,
    )
    .await;
    assert_eq!(live.next_ordinal, 8 * BASE_LENGTH);
    assert_eq!(
        fixture
            .runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(8 * BASE_LENGTH + padding.next_ordinal).unwrap(),
    );

    let base_read = base_work.read_bytes_per_message();
    let base_steps = base_work.writer_vm_steps_per_message();
    let grown_read = grown_work.read_bytes_per_message();
    let grown_steps = grown_work.writer_vm_steps_per_message();
    eprintln!(
        "live-session streamed message ingest: read bytes base={base_read} \
         grown={grown_read}; writer VM steps base={base_steps} grown={grown_steps}; \
         base work={base_work:?}; grown work={grown_work:?}"
    );
    assert!(
        grown_read * 5 <= base_read * 6,
        "an 8x longer live session must keep each streamed message's ingest reads \
         within 1.2x: base={base_read} grown={grown_read}; \
         base work={base_work:?}; grown work={grown_work:?}"
    );
    assert!(
        grown_steps * 5 <= base_steps * 6,
        "an 8x longer live session must keep each streamed message's ingest writer \
         work within 1.2x: base={base_steps} grown={grown_steps}; \
         base work={base_work:?}; grown work={grown_work:?}"
    );
}
