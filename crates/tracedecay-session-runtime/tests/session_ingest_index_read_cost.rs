//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use std::sync::Arc;
use support::{
    BASE_SESSIONS, DrainFixture, GROWN_SESSIONS, MEDIAN_PROBE_MESSAGES, MESSAGES_PER_SESSION,
    ingest_live_messages, median_streamed_message_reads, refresh_until_idle, seed_sessions,
    session_cursor, session_store_tree_depths,
};
use tracedecay_session_runtime::session_sync::test_harness::SessionTemporalRefreshWakeState;
use tracedecay_sessions::admission::HostAdmissionScope;

/// An 8x larger store may only deepen the B-trees a streamed message looks
/// up. A tree of n pages with interior fanout f has 1 + ceil(log_f n) levels,
/// and session-store interior pages hold far more than 8 keys, so 8x the rows
/// adds at most one level to any tree. A point lookup in a tree of d levels
/// reads d pages, so one that gains a level reads at most (d + 1) / d as much,
/// and the worst case over the trees that gained a level, the shallowest of
/// them, bounds a message whose reads are all point lookups. A scan of any
/// store-sized range grows 8x and breaks that bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_message_ingest_reads_grow_only_with_index_depth() {
    const LIVE_LENGTH: u64 = 16;
    const WINDOWS: usize = 5;
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .unwrap();
    let refresh = Arc::new(SessionTemporalRefreshWakeState::default());
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    refresh_until_idle(&database, &refresh).await;
    let mut base_live = session_cursor(GROWN_SESSIONS);
    ingest_live_messages(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut base_live,
        LIVE_LENGTH,
    )
    .await;
    let base_read = median_streamed_message_reads(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut base_live,
        WINDOWS,
    )
    .await;
    let base_depths = session_store_tree_depths(&fixture);

    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    refresh_until_idle(&database, &refresh).await;
    let mut grown_live = session_cursor(GROWN_SESSIONS + 1);
    ingest_live_messages(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut grown_live,
        LIVE_LENGTH,
    )
    .await;
    let grown_read = median_streamed_message_reads(
        &facade,
        &fixture,
        &database,
        &refresh,
        &mut grown_live,
        WINDOWS,
    )
    .await;
    let grown_depths = session_store_tree_depths(&fixture);
    assert_eq!(
        fixture
            .runtime
            .project_session_message_count_for_test()
            .await
            .unwrap(),
        i64::try_from(
            GROWN_SESSIONS * MESSAGES_PER_SESSION
                + 2 * (LIVE_LENGTH + MEDIAN_PROBE_MESSAGES * u64::try_from(WINDOWS).unwrap())
        )
        .unwrap(),
    );

    let mut deepened = Vec::new();
    for (tree, grown) in &grown_depths {
        let base = base_depths.get(tree).copied().unwrap_or(1);
        assert!(
            *grown <= base + 1,
            "an 8x larger store must add at most one level to {tree}: {base} -> {grown}"
        );
        if *grown > base {
            deepened.push((tree.as_str(), base));
        }
    }
    let shallowest = deepened
        .iter()
        .map(|(_, base)| *base)
        .min()
        .unwrap_or(u32::MAX);
    eprintln!(
        "8x store streamed message ingest read bytes: base={base_read} grown={grown_read}; \
         trees that gained a level (base depth): {deepened:?}"
    );
    assert!(
        !deepened.is_empty(),
        "an 8x larger store must deepen a tree"
    );
    assert!(
        grown_read * u64::from(shallowest) <= base_read * u64::from(shallowest + 1),
        "an 8x larger store must grow a streamed message's reads by at most \
         (d + 1) / d for the shallowest deepened tree d={shallowest}: \
         base={base_read} grown={grown_read}"
    );
}
