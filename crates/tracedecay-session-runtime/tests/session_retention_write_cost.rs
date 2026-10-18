//! This probe owns its process so process-wide I/O measurements cannot
//! include another probe's work, and Bazel can schedule it independently.

#![cfg(target_os = "linux")]

#[path = "support/session_store_read_cost.rs"]
pub mod support;

use support::{BASE_SESSIONS, DrainFixture, GROWN_SESSIONS, retention_writer_steps, seed_sessions};
use tracedecay_sessions::admission::HostAdmissionScope;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_write_transactions_do_not_scale_with_the_session_store() {
    let fixture = DrainFixture::open().await;
    let facade = fixture.facade();
    let database = fixture
        .runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let (project, scope) = (fixture.project.as_path(), fixture.scope());

    seed_sessions(&facade, project, &scope, 0..BASE_SESSIONS).await;
    let base_steps = retention_writer_steps(database).await;
    seed_sessions(&facade, project, &scope, BASE_SESSIONS..GROWN_SESSIONS).await;
    let grown_steps = retention_writer_steps(database).await;

    eprintln!("retention writer VM steps: base={base_steps} grown={grown_steps}");
    assert!(
        grown_steps <= base_steps * 2,
        "an 8x larger session store must not double the work retention does inside \
         its write transactions: base={base_steps} grown={grown_steps}"
    );
}
