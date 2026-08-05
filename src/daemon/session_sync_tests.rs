use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionSyncCommandV1, SessionSyncCompletionReceiptV1, SessionSyncCoverageV1,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncSourceCoverageV1, SessionSyncStatsV1, SessionTranscriptImportV1,
};
use tracedecay_application::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId,
};

use super::{
    DaemonSessionSyncService, completed_profile_sweep_covers, completion_termination,
    decode_matching_journal,
};
use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

#[test]
fn committed_result_stays_terminal_when_cancel_or_deadline_arrives_late() {
    let stats = SessionSyncStatsV1 {
        sessions_imported: 1,
        messages_imported: 3,
        ..SessionSyncStatsV1::default()
    };

    assert_eq!(
        completion_termination(true, &stats, true, true),
        OperationTermination::Completed
    );
    assert_eq!(
        completion_termination(true, &stats, true, false),
        OperationTermination::Partial
    );
}

#[test]
fn uncommitted_failure_is_not_reported_as_success() {
    assert_eq!(
        completion_termination(false, &SessionSyncStatsV1::default(), true, false),
        OperationTermination::Failed
    );
}

#[test]
fn partial_coverage_is_never_completed_without_failures() {
    assert_eq!(
        completion_termination(false, &SessionSyncStatsV1::default(), false, true),
        OperationTermination::Failed
    );
    let stats = SessionSyncStatsV1 {
        messages_imported: 1,
        ..SessionSyncStatsV1::default()
    };
    assert_eq!(
        completion_termination(true, &stats, false, true),
        OperationTermination::Partial
    );
}

#[test]
fn completed_profile_sweep_only_covers_already_admitted_work() {
    assert!(completed_profile_sweep_covers(
        Some(&UtcMicros(20)),
        UtcMicros(19)
    ));
    assert!(!completed_profile_sweep_covers(
        Some(&UtcMicros(20)),
        UtcMicros(21)
    ));
    assert!(!completed_profile_sweep_covers(None, UtcMicros(19)));
}

#[test]
fn completed_alias_replay_survives_its_original_deadline() {
    let request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.alias").unwrap(),
        IdempotencyKey::new("session-sync.alias").unwrap(),
        SessionSyncScopeV1::new(
            ProjectId::new("project.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        ),
        Deadline::new(UtcMicros(20)).unwrap(),
        CancellationSignal::active("session-sync.alias").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let primary = IdempotencyKey::new("session-sync.primary").unwrap();
    let mut journal = SessionSyncJournalV1::coalesced(&request, UtcMicros(10), primary.clone());
    journal.status = SessionSyncJournalStatusV1::Complete;
    journal.completion = Some(SessionSyncCompletionReceiptV1 {
        admission: journal.admission.clone(),
        coalesced_primary: Some(primary),
        completed_at: UtcMicros(15),
        termination: OperationTermination::Completed,
        stats: SessionSyncStatsV1::default(),
        coverage: vec![SessionSyncSourceCoverageV1 {
            store_scope: "profile".to_owned(),
            coverage: SessionSyncCoverageV1::Complete,
        }],
        source_frontiers: Vec::new(),
        failure_codes: Vec::new(),
    });

    assert!(request.admit_at(UtcMicros(30)).is_err());
    let encoded = serde_json::to_string(&journal).unwrap();
    assert!(matches!(
        decode_matching_journal(&encoded, &request)
            .unwrap()
            .outcome(),
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.admission.idempotency_key == *request.idempotency_key()
                && receipt.coalesced_primary.is_some()
    ));
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
