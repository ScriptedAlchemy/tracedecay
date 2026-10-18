//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use support::{
    BASE_SESSIONS, DrainFixture, GROWN_SESSIONS, SEED_TIMESTAMP, probe_one_message,
    retention_tick_read_bytes, seed_sessions,
};
use tracedecay_sessions::admission::HostAdmissionScope;

/// Every retention tick after the store settles follows the messages streamed
/// since the previous tick. Payload GC, observation release, session
/// retention, and observability pruning each select from their own cursors or
/// candidate indexes, so an 8x larger store costs the same tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_tick_reads_do_not_scale_with_the_session_store() {
    const STREAMED: u64 = 64;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());
    let probe_timestamp = SEED_TIMESTAMP + 10_000_000;

    let mut streamed = seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    retention_tick_read_bytes(database).await;
    for offset in 0..STREAMED {
        probe_one_message(
            &facade,
            project,
            &scope,
            &mut streamed,
            probe_timestamp + i64::try_from(offset).unwrap(),
        )
        .await;
    }
    let base_read = retention_tick_read_bytes(database).await;

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    retention_tick_read_bytes(database).await;
    for offset in STREAMED..2 * STREAMED {
        probe_one_message(
            &facade,
            project,
            &scope,
            &mut streamed,
            probe_timestamp + i64::try_from(offset).unwrap(),
        )
        .await;
    }
    let grown_read = retention_tick_read_bytes(database).await;

    eprintln!("retention tick read bytes: base={base_read} grown={grown_read}");
    assert!(
        grown_read * 10 <= base_read * 12,
        "one retention tick on an 8x larger session store must stay within 1.2x of the \
         base store's reads: base={base_read} grown={grown_read}"
    );
}
