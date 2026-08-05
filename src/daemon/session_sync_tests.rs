use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::OperationTermination;
use tracedecay_application::session_sync::SessionSyncStatsV1;

use super::{DaemonSessionSyncService, completion_termination};

#[test]
fn committed_result_stays_terminal_when_cancel_or_deadline_arrives_late() {
    let stats = SessionSyncStatsV1 {
        sessions_imported: 1,
        messages_imported: 3,
        ..SessionSyncStatsV1::default()
    };

    assert_eq!(
        completion_termination(true, &stats, true),
        OperationTermination::Completed
    );
    assert_eq!(
        completion_termination(true, &stats, false),
        OperationTermination::Partial
    );
}

#[test]
fn uncommitted_failure_is_not_reported_as_success() {
    assert_eq!(
        completion_termination(false, &SessionSyncStatsV1::default(), false),
        OperationTermination::Failed
    );
}

#[tokio::test]
async fn daemon_wide_scan_slot_serializes_concurrent_acquisition() {
    let service = DaemonSessionSyncService::default();
    let first = service.scan_slots.clone().acquire_owned().await.unwrap();
    let second_slots = Arc::clone(&service.scan_slots);
    let mut second = tokio::spawn(async move { second_slots.acquire_owned().await.unwrap() });

    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut second)
            .await
            .is_err(),
        "a second native scan must remain queued while the daemon-wide slot is held"
    );
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut second)
            .await
            .unwrap()
            .is_ok()
    );
}
