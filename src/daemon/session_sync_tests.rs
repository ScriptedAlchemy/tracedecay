use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionSyncCommandV1, SessionSyncCompletionReceiptV1, SessionSyncCoverageV1,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionSyncSourceCoverageV1, SessionSyncStatsV1,
    SessionTranscriptImportV1,
};
use tracedecay_application::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId,
};

use super::git_topology::GitTopologySyncFailure;
use super::work::{
    SessionSyncInterruption, coalesced_alias_local_interruption, git_history_frontier_from_meta,
    git_history_source_frontier, git_sync_with_topology_result, git_sync_work_result,
};
use super::{
    DaemonSessionSyncConfig, DaemonSessionSyncService, SessionSyncWorkResult,
    completed_profile_sweep_covers, completion_termination, decode_matching_journal, journal_key,
};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};

#[test]
fn cancel_after_first_git_commit_preserves_progress_and_cancelled_termination() {
    let result = git_sync_work_result(
        &ProjectId::new("project.cancel-after-commit").unwrap(),
        tracedecay_sessions::runtime::git_correlation::BoundedBackfillOutcome {
            stats: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                sessions_scanned: 1,
                spans_written: 2,
                commits_attributed: 3,
                ..tracedecay_sessions::runtime::git_correlation::BackfillStats::default()
            },
            committed: true,
            frontier: tracedecay_sessions::runtime::git_correlation::GitHistoryIndexFrontier {
                activity_timestamp: 1_723_456_789,
                source_rowid: 417,
            },
            remaining_sessions: 1,
            unresolved_failures: 0,
            interruption: Some(
                tracedecay_sessions::runtime::git_correlation::BoundedBackfillInterruption::Cancelled,
            ),
        },
        Some(SessionSyncInterruption::Cancelled),
    );
    let SessionSyncWorkResult::Finished {
        interruption,
        committed,
        stats,
        coverage,
        source_frontiers,
        failure_codes,
    } = result
    else {
        panic!("committed Git progress must produce durable terminal evidence");
    };

    assert!(committed);
    assert_eq!(stats.sessions_scanned, 1);
    assert_eq!(stats.spans_written, 2);
    assert_eq!(stats.commits_attributed, 3);
    assert_eq!(
        coverage,
        vec![SessionSyncSourceCoverageV1 {
            store_scope: "git".to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
        }]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&source_frontiers[0].committed_cursor_json)
            .unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_789,
            "source_rowid": 417,
        })
    );
    assert_eq!(
        failure_codes,
        vec!["git_sync_cancelled_after_commit".to_owned()]
    );
    assert_eq!(
        completion_termination(
            interruption.and_then(SessionSyncInterruption::termination),
            committed,
            &stats,
            false,
            failure_codes.is_empty(),
        ),
        OperationTermination::Cancelled
    );
}

#[test]
fn deadline_after_first_git_commit_preserves_progress_and_timed_out_termination() {
    let result = git_sync_work_result(
        &ProjectId::new("project.deadline-after-commit").unwrap(),
        tracedecay_sessions::runtime::git_correlation::BoundedBackfillOutcome {
            stats: tracedecay_sessions::runtime::git_correlation::BackfillStats {
                sessions_scanned: 1,
                spans_written: 2,
                commits_attributed: 3,
                ..tracedecay_sessions::runtime::git_correlation::BackfillStats::default()
            },
            committed: true,
            frontier: tracedecay_sessions::runtime::git_correlation::GitHistoryIndexFrontier {
                activity_timestamp: 1_723_456_790,
                source_rowid: 418,
            },
            remaining_sessions: 0,
            unresolved_failures: 0,
            interruption: Some(
                tracedecay_sessions::runtime::git_correlation::BoundedBackfillInterruption::Cancelled,
            ),
        },
        Some(SessionSyncInterruption::TimedOut),
    );
    let SessionSyncWorkResult::Finished {
        interruption,
        committed,
        stats,
        coverage,
        source_frontiers,
        failure_codes,
    } = result
    else {
        panic!("committed Git progress must produce durable terminal evidence");
    };

    assert!(committed);
    assert_eq!(stats.sessions_scanned, 1);
    assert_eq!(stats.spans_written, 2);
    assert_eq!(stats.commits_attributed, 3);
    assert_eq!(
        coverage,
        vec![SessionSyncSourceCoverageV1 {
            store_scope: "git".to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
        }]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&source_frontiers[0].committed_cursor_json)
            .unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_790,
            "source_rowid": 418,
        })
    );
    assert_eq!(
        failure_codes,
        vec!["git_sync_timed_out_after_commit".to_owned()]
    );
    assert_eq!(
        completion_termination(
            interruption.and_then(SessionSyncInterruption::termination),
            committed,
            &stats,
            false,
            failure_codes.is_empty(),
        ),
        OperationTermination::TimedOut
    );
}

#[test]
fn committed_result_stays_terminal_when_cancel_or_deadline_arrives_late() {
    let stats = SessionSyncStatsV1 {
        sessions_imported: 1,
        messages_imported: 3,
        ..SessionSyncStatsV1::default()
    };

    assert_eq!(
        completion_termination(None, true, &stats, true, true),
        OperationTermination::Completed
    );
    assert_eq!(
        completion_termination(None, true, &stats, true, false),
        OperationTermination::Partial
    );
}

#[test]
fn uncommitted_failure_is_not_reported_as_success() {
    assert_eq!(
        completion_termination(None, false, &SessionSyncStatsV1::default(), true, false,),
        OperationTermination::Failed
    );
}

#[test]
fn partial_coverage_is_never_completed_without_failures() {
    assert_eq!(
        completion_termination(None, false, &SessionSyncStatsV1::default(), false, true,),
        OperationTermination::Failed
    );
    let stats = SessionSyncStatsV1 {
        messages_imported: 1,
        ..SessionSyncStatsV1::default()
    };
    assert_eq!(
        completion_termination(None, true, &stats, false, true),
        OperationTermination::Partial
    );
}

#[test]
fn declared_git_topology_failures_keep_their_typed_failure_code() {
    for (failure, expected) in [
        (
            GitTopologySyncFailure::Stale,
            "git_topology_declared_state_stale",
        ),
        (
            GitTopologySyncFailure::Denied,
            "git_topology_declared_authority_denied",
        ),
        (
            GitTopologySyncFailure::Unavailable,
            "git_topology_declared_authority_unavailable",
        ),
    ] {
        let result = git_sync_with_topology_result(
            SessionSyncWorkResult::Finished {
                interruption: None,
                committed: true,
                stats: SessionSyncStatsV1::default(),
                coverage: Vec::new(),
                source_frontiers: Vec::new(),
                failure_codes: Vec::new(),
            },
            Err(failure),
        );
        let SessionSyncWorkResult::Finished { failure_codes, .. } = result else {
            panic!("declared topology failure must preserve completed Git sync evidence");
        };
        assert_eq!(failure_codes, vec![expected.to_owned()]);
    }
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
    let pending_alias = journal.clone();
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
    assert_eq!(
        coalesced_alias_local_interruption(&journal, &pending_alias, true, UtcMicros(30)),
        None,
        "the primary terminal receipt wins over alias-local timeout and cancellation"
    );
}

#[test]
fn git_recovery_frontier_preserves_the_exact_committed_tuple() {
    let frontier = git_history_frontier_from_meta(Some(1_723_456_789), Some(417)).unwrap();

    assert_eq!(frontier.activity_timestamp, 1_723_456_789);
    assert_eq!(frontier.source_rowid, 417);
    let receipt_frontier =
        git_history_source_frontier(&ProjectId::new("project.fixture").unwrap(), frontier);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&receipt_frontier.committed_cursor_json).unwrap(),
        serde_json::json!({
            "activity_timestamp": 1_723_456_789,
            "source_rowid": 417,
        })
    );
    assert!(git_history_frontier_from_meta(None, Some(999)).is_none());
}

#[tokio::test]
async fn cancel_in_alias_activation_gap_mirrors_primary_terminal_receipt() {
    let profile_root = tempfile::tempdir().unwrap();
    let project_root = profile_root.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let project_id = ProjectId::new("project.cancel-alias-race").unwrap();
    let profile_id = UserProfileId::new("profile.cancel-alias-race").unwrap();
    let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        profile_root.path(),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let project_sessions = runtime
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Profile)
        .unwrap();
    let service = DaemonSessionSyncService::default();
    service
        .register_project(DaemonSessionSyncConfig {
            brain_id: BrainId::new("brain.cancel-alias-race").unwrap(),
            profile_id: profile_id.clone(),
            project_id: project_id.clone(),
            profile_root: profile_root.path().to_path_buf(),
            project_root,
            transcript_source_home: None,
            project_sessions,
            user_sessions: Arc::clone(&profile_sessions),
            registry: Arc::clone(&profile_sessions),
            startup_import: false,
            project_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
            user_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();
    let scope = SessionSyncScopeV1::new(project_id, profile_id);
    let primary_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.cancel-primary").unwrap(),
        IdempotencyKey::new("session-sync.cancel-primary").unwrap(),
        scope.clone(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.cancel-primary").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let mut primary = SessionSyncJournalV1::queued(&primary_request, UtcMicros(10));
    primary.status = SessionSyncJournalStatusV1::Complete;
    primary.completion = Some(SessionSyncCompletionReceiptV1 {
        admission: primary.admission.clone(),
        coalesced_primary: None,
        completed_at: UtcMicros(20),
        termination: OperationTermination::Completed,
        stats: SessionSyncStatsV1 {
            sessions_imported: 1,
            ..SessionSyncStatsV1::default()
        },
        coverage: vec![SessionSyncSourceCoverageV1 {
            store_scope: "project".to_owned(),
            coverage: SessionSyncCoverageV1::Complete,
        }],
        source_frontiers: Vec::new(),
        failure_codes: Vec::new(),
    });
    let alias_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.cancel-alias").unwrap(),
        IdempotencyKey::new("session-sync.cancel-alias").unwrap(),
        scope.clone(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.cancel-alias").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let alias = SessionSyncJournalV1::coalesced(
        &alias_request,
        UtcMicros(11),
        primary_request.idempotency_key().clone(),
    );
    let primary_key = journal_key(&scope, primary_request.idempotency_key());
    let alias_key = journal_key(&scope, alias_request.idempotency_key());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let cancel_service = service.clone();
    let cancel_barrier = Arc::clone(&barrier);
    let control = tracedecay_application::session_sync::SessionSyncControlV1::new(
        scope,
        alias_request.idempotency_key().clone(),
    );
    let cancel = tokio::spawn(async move {
        cancel_barrier.wait().await;
        SessionSyncServicePort::cancel(&cancel_service, control).await
    });

    assert!(
        profile_sessions
            .insert_session_sync_journal(&primary_key, &serde_json::to_string(&primary).unwrap(),)
            .await
            .unwrap()
    );
    assert!(
        profile_sessions
            .insert_session_sync_journal(&alias_key, &serde_json::to_string(&alias).unwrap())
            .await
            .unwrap()
    );
    assert!(!service.active_contains(&alias_key));
    barrier.wait().await;

    assert!(matches!(
        cancel.await.unwrap(),
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.termination == OperationTermination::Completed
                && receipt.admission.idempotency_key == *alias_request.idempotency_key()
                && receipt.coalesced_primary
                    == Some(primary_request.idempotency_key().clone())
    ));
    let persisted: SessionSyncJournalV1 = serde_json::from_str(
        &profile_sessions
            .read_session_sync_journal(&alias_key)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(persisted.cancel_requested_at.is_none());
    assert_eq!(
        persisted.completion.unwrap().termination,
        OperationTermination::Completed
    );
    drop(runtime);
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
