//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use support::{
    BACKLOG_SESSIONS, BASE_BACKLOG_MESSAGES, DrainFixture, GROWN_BACKLOG_MESSAGES,
    backlog_drain_read_per_message,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_backlog_drain_reads_scale_with_the_backlog() {
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    let base = backlog_drain_read_per_message(
        &facade,
        project,
        &scope,
        0..BACKLOG_SESSIONS,
        BASE_BACKLOG_MESSAGES,
    )
    .await;
    let grown = backlog_drain_read_per_message(
        &facade,
        project,
        &scope,
        BACKLOG_SESSIONS..2 * BACKLOG_SESSIONS,
        GROWN_BACKLOG_MESSAGES,
    )
    .await;

    eprintln!("backlog drain read bytes per message: base={base} grown={grown}");
    assert!(
        grown <= base * 2,
        "draining a 16x larger backlog must not double each message's reads: \
         base={base} grown={grown}"
    );
}
