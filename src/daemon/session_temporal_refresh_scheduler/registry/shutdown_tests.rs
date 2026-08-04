use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{
    SessionTemporalRefreshSchedulerEntry, SessionTemporalRefreshSchedulerRegistry,
    SessionTemporalRefreshWakeState, inert_session_temporal_refresh_wake,
};
use crate::daemon::shutdown_coordination::ShutdownStatus;

#[tokio::test]
async fn shutdown_until_preserves_worker_task_panic() {
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    registry.profile.lock().await.insert(
        PathBuf::from("panicked-session-refresh"),
        SessionTemporalRefreshSchedulerEntry {
            state: Arc::new(SessionTemporalRefreshWakeState::default()),
            wake: inert_session_temporal_refresh_wake(),
            task: tokio::spawn(async {
                panic!("session refresh shutdown task panic");
            }),
        },
    );

    let status = registry
        .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(10))
        .await;

    assert!(matches!(
        status,
        ShutdownStatus::Failed(error) if error.contains("session refresh shutdown task panic")
    ));
}
