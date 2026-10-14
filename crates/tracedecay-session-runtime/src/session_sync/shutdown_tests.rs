use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use tracedecay_contracts::CancellationSignal;
use tracedecay_contracts::session_sync::{SessionSyncScopeV1, SessionSyncServicePort};
use tracedecay_domain::{ProjectId, UserProfileId};
use tracedecay_runtime_core::native_runtime::{self, NativeTask};

use crate::session_sync::{DaemonSessionSyncService, SessionSyncTaskV1, test_harness};

/// The daemon coordinator drops the `session_sync` join at its phase deadline,
/// which can come before the join's own abort grace ends. A task that ignores
/// its cancellation must be aborted then. Its handle still returns to the
/// service, so a later shutdown joins it.
#[tokio::test]
async fn timed_out_shutdown_aborts_the_session_sync_task() {
    let service = DaemonSessionSyncService::default();
    let (alive, task_dropped) = std::sync::mpsc::channel::<()>();
    let task = NativeTask::spawn(async move {
        let _alive = alive;
        std::future::pending::<()>().await;
    })
    .expect("native task");
    test_harness::push_task(
        &service,
        SessionSyncTaskV1 {
            scope: SessionSyncScopeV1::new(
                ProjectId::new("project.session-sync.timed-out-owner").expect("project id"),
                UserProfileId::new("profile.session-sync.timed-out-owner").expect("profile id"),
            ),
            key: "session-sync.timed-out-owner".to_owned(),
            cancellation: CancellationSignal::active("session-sync.timed-out-owner")
                .expect("cancellation"),
            task,
        },
    );

    assert!(
        native_runtime::timeout(
            Duration::from_millis(100),
            SessionSyncServicePort::shutdown(&service),
        )
        .await
        .is_err(),
        "the join must still be waiting inside its abort grace"
    );

    assert_eq!(
        task_dropped.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "a timed-out shutdown must not leave the session sync task running"
    );
    assert_eq!(
        test_harness::task_count(&service),
        1,
        "the aborted task returns to the service for a later join"
    );
    native_runtime::timeout(
        Duration::from_secs(5),
        SessionSyncServicePort::shutdown(&service),
    )
    .await
    .expect("a later shutdown joins the aborted task");
    assert_eq!(test_harness::task_count(&service), 0);
}
