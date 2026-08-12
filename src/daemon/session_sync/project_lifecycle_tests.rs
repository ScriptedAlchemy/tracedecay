use std::sync::{Arc, PoisonError};
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
use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};

use super::project_lifecycle::SessionSyncTaskV1;
use super::{DaemonSessionSyncConfig, DaemonSessionSyncService};

fn request(project_id: ProjectId, profile_id: UserProfileId) -> SessionSyncRequestV1 {
    SessionSyncRequestV1::new(
        RequestId::new(format!("session-sync.retirement.{}", project_id.as_str())).unwrap(),
        IdempotencyKey::new(format!("session-sync.retirement.{}", project_id.as_str())).unwrap(),
        SessionSyncScopeV1::new(project_id, profile_id),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.retirement.request").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    )
}

async fn register(
    service: &DaemonSessionSyncService,
    root: &tempfile::TempDir,
    project_id: ProjectId,
) -> (
    crate::host_admission::HostAdmissionTestRuntimeV1,
    Arc<crate::global_db::RegisteredGlobalDb>,
    UserProfileId,
) {
    let project_root = root.path().join(project_id.as_str());
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        root.path(),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let project_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Profile)
        .unwrap();
    let brain_id = project_sessions.binding().shard_id.brain_id.clone();
    let profile_id = project_sessions.binding().shard_id.profile_id.clone();
    service
        .register_project(DaemonSessionSyncConfig {
            brain_id,
            profile_id: profile_id.clone(),
            project_id,
            profile_root: root.path().to_path_buf(),
            project_root,
            transcript_source_home: None,
            project_sessions: Arc::clone(&project_sessions),
            user_sessions: Arc::clone(&profile_sessions),
            registry: profile_sessions,
            startup_import: false,
            project_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
            user_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();
    (runtime, project_sessions, profile_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exact_project_retirement_drains_a_keeps_b_live_and_rebinds_a() {
    let service = DaemonSessionSyncService::default();
    let project_a = ProjectId::new("project.session-sync.retirement-a").unwrap();
    let project_b = project_a.clone();
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let (_runtime_a, old_a, profile_a) = register(&service, &root_a, project_a.clone()).await;
    let (_runtime_b, database_b, profile_b) = register(&service, &root_b, project_b.clone()).await;

    let cancellation_a = CancellationSignal::active("session-sync.retire-a").unwrap();
    let cancellation_b = CancellationSignal::active("session-sync.retire-b").unwrap();
    let cancelled_a = Arc::new(tokio::sync::Notify::new());
    let release_a = Arc::new(tokio::sync::Notify::new());
    let release_b = Arc::new(tokio::sync::Notify::new());
    let task_a_cancellation = cancellation_a.clone();
    let task_a_cancelled = Arc::clone(&cancelled_a);
    let task_a_release = Arc::clone(&release_a);
    let task_a = tokio::spawn(async move {
        while !task_a_cancellation.is_cancelled() {
            tokio::task::yield_now().await;
        }
        task_a_cancelled.notify_one();
        task_a_release.notified().await;
    });
    let task_b_release = Arc::clone(&release_b);
    let task_b = tokio::spawn(async move {
        task_b_release.notified().await;
    });
    service
        .tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend([
            SessionSyncTaskV1 {
                scope: SessionSyncScopeV1::new(project_a.clone(), profile_a.clone()),
                key: "session-sync.retire-a".to_owned(),
                cancellation: cancellation_a,
                task: task_a,
            },
            SessionSyncTaskV1 {
                scope: SessionSyncScopeV1::new(project_b.clone(), profile_b.clone()),
                key: "session-sync.retire-b".to_owned(),
                cancellation: cancellation_b.clone(),
                task: task_b,
            },
        ]);

    let retire_service = service.clone();
    let retire_project = project_a.clone();
    let retire_profile = profile_a.clone();
    let retirement = tokio::spawn(async move {
        retire_service
            .retire_project(&retire_profile, &retire_project)
            .await
    });
    cancelled_a.notified().await;
    let scope_a = SessionSyncScopeV1::new(project_a.clone(), profile_a.clone());
    let scope_b = SessionSyncScopeV1::new(project_b.clone(), profile_b);
    assert!(
        service
            .context_for(&scope_a)
            .unwrap()
            .project_sessions()
            .is_err()
    );
    assert!(Arc::ptr_eq(
        &service
            .context_for(&scope_b)
            .unwrap()
            .project_sessions()
            .unwrap(),
        &database_b
    ));
    assert!(!cancellation_b.is_cancelled());

    let unavailable_service = service.clone();
    let unavailable_project = project_a.clone();
    let unavailable_profile = profile_a.clone();
    let unavailable = tokio::spawn(async move {
        SessionSyncServicePort::execute(
            &unavailable_service,
            request(unavailable_project, unavailable_profile),
        )
        .await
    });
    tokio::task::yield_now().await;
    assert!(!unavailable.is_finished());
    release_a.notify_one();
    assert!(retirement.await.unwrap().unwrap());
    assert!(matches!(
        unavailable.await.unwrap(),
        SessionSyncOutcomeV1::Unavailable {
            reason_code: "session_sync_project_retired"
        }
    ));

    assert!(
        service
            .rebind_project(&profile_a, &project_a, &database_b)
            .await
            .is_err()
    );

    let replacement_a = Arc::new(
        crate::global_db::RegisteredGlobalDb::migrate_and_attach(
            old_a.runtime().clone(),
            old_a.binding().clone(),
            old_a.runtime().locator().verified().clone(),
            old_a.authority().clone(),
        )
        .await
        .unwrap(),
    );
    let recovery_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.rebind-recovery").unwrap(),
        IdempotencyKey::new("session-sync.rebind-recovery").unwrap(),
        scope_a.clone(),
        Deadline::new(UtcMicros(1)).unwrap(),
        CancellationSignal::active("session-sync.rebind-recovery").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let recovery_key = super::journal_key(&scope_a, recovery_request.idempotency_key());
    let recovery_journal = SessionSyncJournalV1::queued(&recovery_request, UtcMicros(0));
    service
        .context_for(&scope_a)
        .unwrap()
        .registry
        .insert_session_sync_journal(
            &recovery_key,
            &serde_json::to_string(&recovery_journal).unwrap(),
        )
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&old_a, &replacement_a));
    assert_eq!(old_a.binding(), replacement_a.binding());
    assert!(
        service
            .rebind_project(&profile_a, &project_a, &replacement_a)
            .await
            .unwrap()
    );
    assert!(Arc::ptr_eq(
        &service
            .context_for(&scope_a)
            .unwrap()
            .project_sessions()
            .unwrap(),
        &replacement_a
    ));
    let recovered: SessionSyncJournalV1 = serde_json::from_str(
        &service
            .context_for(&scope_a)
            .unwrap()
            .registry
            .read_session_sync_journal(&recovery_key)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(recovered.status, SessionSyncJournalStatusV1::Complete);
    assert_eq!(
        recovered.completion.unwrap().termination,
        OperationTermination::TimedOut
    );

    assert!(
        service
            .retire_project(&profile_a, &project_a)
            .await
            .unwrap()
    );
    let replay = SessionSyncServicePort::cancel(
        &service,
        tracedecay_application::session_sync::SessionSyncControlV1::new(
            scope_a.clone(),
            recovery_request.idempotency_key().clone(),
        ),
    )
    .await;
    assert!(matches!(
        replay,
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.termination == OperationTermination::TimedOut
    ));

    release_b.notify_one();
    SessionSyncServicePort::shutdown(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registration_recovery_fences_concurrent_execute() {
    let service = DaemonSessionSyncService::default();
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.session-sync.registration-race").unwrap();
    let project_root = root.path().join(project_id.as_str());
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        root.path(),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let project_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Profile)
        .unwrap();
    let brain_id = project_sessions.binding().shard_id.brain_id.clone();
    let profile_id = project_sessions.binding().shard_id.profile_id.clone();
    let profile_root = root.path().to_path_buf();
    let request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.registration-race").unwrap(),
        IdempotencyKey::new("session-sync.registration-race").unwrap(),
        SessionSyncScopeV1::new(project_id.clone(), profile_id.clone()),
        Deadline::new(UtcMicros(1)).unwrap(),
        CancellationSignal::active("session-sync.registration-race").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let scope = request.scope().clone();
    let key = super::journal_key(&scope, request.idempotency_key());
    profile_sessions
        .insert_session_sync_journal(
            &key,
            &serde_json::to_string(&SessionSyncJournalV1::queued(&request, UtcMicros(0))).unwrap(),
        )
        .await
        .unwrap();

    let gate = service.project_gate(&scope);
    let held = gate.lock().await;
    let registration_service = service.clone();
    let mut registration = tokio::spawn(async move {
        registration_service
            .register_project(DaemonSessionSyncConfig {
                brain_id,
                profile_id,
                project_id,
                profile_root,
                project_root,
                transcript_source_home: None,
                project_sessions,
                user_sessions: Arc::clone(&profile_sessions),
                registry: profile_sessions,
                startup_import: false,
                project_refresh:
                    crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
                user_refresh:
                    crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
            })
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut registration)
            .await
            .is_err()
    );
    let execute_service = service.clone();
    let mut execute =
        tokio::spawn(
            async move { SessionSyncServicePort::execute(&execute_service, request).await },
        );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut execute)
            .await
            .is_err()
    );
    drop(held);

    registration.await.unwrap().unwrap();
    assert!(matches!(
        execute.await.unwrap(),
        SessionSyncOutcomeV1::Complete(receipt)
            if receipt.termination == OperationTermination::TimedOut
    ));
    SessionSyncServicePort::shutdown(&service).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_recovered_alias_does_not_suppress_startup_import() {
    let service = DaemonSessionSyncService::default();
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.session-sync.terminal-alias").unwrap();
    let project_root = root.path().join(project_id.as_str());
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        root.path(),
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    let project_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Profile)
        .unwrap();
    let brain_id = project_sessions.binding().shard_id.brain_id.clone();
    let profile_id = project_sessions.binding().shard_id.profile_id.clone();
    let scope = SessionSyncScopeV1::new(project_id.clone(), profile_id.clone());
    let primary_request = request(project_id.clone(), profile_id.clone());
    let mut primary = SessionSyncJournalV1::queued(&primary_request, UtcMicros(1));
    primary.status = SessionSyncJournalStatusV1::Complete;
    primary.completion = Some(SessionSyncCompletionReceiptV1 {
        admission: primary.admission.clone(),
        coalesced_primary: None,
        completed_at: UtcMicros(2),
        termination: OperationTermination::Completed,
        stats: SessionSyncStatsV1::default(),
        coverage: vec![SessionSyncSourceCoverageV1 {
            store_scope: "project".to_owned(),
            coverage: SessionSyncCoverageV1::Complete,
        }],
        source_frontiers: Vec::new(),
        failure_codes: Vec::new(),
    });
    let alias_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.terminal-alias").unwrap(),
        IdempotencyKey::new("session-sync.terminal-alias").unwrap(),
        scope.clone(),
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("session-sync.terminal-alias").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let alias = SessionSyncJournalV1::coalesced(
        &alias_request,
        UtcMicros(1),
        primary_request.idempotency_key().clone(),
    );
    for (key, journal) in [
        (
            super::journal_key(&scope, primary_request.idempotency_key()),
            primary,
        ),
        (
            super::journal_key(&scope, alias_request.idempotency_key()),
            alias,
        ),
    ] {
        profile_sessions
            .insert_session_sync_journal(&key, &serde_json::to_string(&journal).unwrap())
            .await
            .unwrap();
    }
    service
        .register_project(DaemonSessionSyncConfig {
            brain_id,
            profile_id,
            project_id,
            profile_root: root.path().to_path_buf(),
            project_root,
            transcript_source_home: None,
            project_sessions,
            user_sessions: Arc::clone(&profile_sessions),
            registry: Arc::clone(&profile_sessions),
            startup_import: true,
            project_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
            user_refresh:
                crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();

    let journals = profile_sessions
        .list_session_sync_journals(&super::journal_prefix(&scope))
        .await
        .unwrap();
    assert!(journals.iter().any(|(_, encoded)| {
        serde_json::from_str::<SessionSyncJournalV1>(encoded).is_ok_and(|journal| {
            journal
                .admission
                .idempotency_key
                .as_str()
                .starts_with("session-sync.startup.")
        })
    }));
    SessionSyncServicePort::shutdown(&service).await;
}
