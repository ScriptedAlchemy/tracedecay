use std::sync::atomic::{AtomicBool, Ordering};
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
use tracedecay_sessions::admission::SESSION_INGEST_DISABLED_REASON_V1;

use tracedecay_session_runtime::session_sync::test_harness::{
    context_count, extend_tasks, journal_key, journal_prefix, push_task, task_count,
};
use tracedecay_session_runtime::session_sync::{
    DaemonSessionSyncConfig, DaemonSessionSyncService, SessionSyncTaskV1,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;

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
    tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Profile)
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
            project_sessions: project_sessions.clone(),
            user_sessions: profile_sessions.clone(),
            registry: profile_sessions,
            startup_import: false,
            project_refresh: SessionTemporalRefreshWake::unavailable(),
            user_refresh: SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();
    (runtime, project_sessions, profile_id)
}

#[tokio::test]
async fn shutdown_releases_registered_project_database_contexts() {
    let service = DaemonSessionSyncService::default();
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.session-sync.shutdown-context").unwrap();
    let (_runtime, _project_sessions, _profile_id) = register(&service, &root, project_id).await;

    assert_eq!(context_count(&service), 1);
    service.shutdown().await;
    assert_eq!(context_count(&service), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_keeps_blocked_lease_task_owned_until_it_exits() {
    struct RetainedSessionLease {
        _databases: [tracedecay_global_db::RegisteredGlobalDbLeaseV1; 2],
        released: Arc<AtomicBool>,
    }

    impl Drop for RetainedSessionLease {
        fn drop(&mut self) {
            self.released.store(true, Ordering::Release);
        }
    }

    fn release_task(release: &Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>) {
        let (released, changed) = &**release;
        *released.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }

    let service = DaemonSessionSyncService::default();
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.session-sync.shutdown-task-owner").unwrap();
    let (runtime, project_sessions, profile_id) =
        register(&service, &root, project_id.clone()).await;
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Profile)
        .unwrap();
    let session_registry = runtime.session_registry_for_test();
    let cancellation = CancellationSignal::active("session-sync.shutdown-task-owner").unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let task_released = Arc::clone(&released);
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let task_release = Arc::clone(&release);
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let task = tokio::task::spawn_blocking(move || {
        let _lease = RetainedSessionLease {
            _databases: [project_sessions, profile_sessions],
            released: task_released,
        };
        let _ = entered_tx.send(());
        let (released, changed) = &*task_release;
        let mut released = released.lock().unwrap_or_else(PoisonError::into_inner);
        while !*released {
            released = changed
                .wait(released)
                .unwrap_or_else(PoisonError::into_inner);
        }
    });
    push_task(
        &service,
        SessionSyncTaskV1 {
            scope: SessionSyncScopeV1::new(project_id, profile_id),
            key: "session-sync.shutdown-task-owner".to_owned(),
            cancellation,
            task,
        },
    );
    entered_rx.await.unwrap();

    let shutdown_service = service.clone();
    let mut shutdown = tokio::spawn(async move {
        SessionSyncServicePort::shutdown(&shutdown_service).await;
    });
    let returned_while_task_owned =
        tokio::time::timeout(Duration::from_millis(2_250), &mut shutdown)
            .await
            .is_ok();
    if returned_while_task_owned {
        release_task(&release);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !released.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        panic!("session sync shutdown detached a task that still owned a registered session lease");
    }

    assert!(!released.load(Ordering::Acquire));
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    assert_eq!(
        task_count(&service),
        1,
        "cancelling the shutdown waiter must return unfinished task ownership to the service"
    );
    assert_eq!(
        context_count(&service),
        1,
        "session contexts must remain mounted until every lease-owning task joins"
    );

    let retry_service = service.clone();
    let retry = tokio::spawn(async move {
        SessionSyncServicePort::shutdown(&retry_service).await;
    });
    tokio::task::yield_now().await;
    assert!(!retry.is_finished());
    release_task(&release);
    retry.await.unwrap();
    assert!(released.load(Ordering::Acquire));
    assert_eq!(context_count(&service), 0);
    drop(service);
    drop(runtime);
    session_registry.cancel_memory_graph_reconciliation_tasks();
    session_registry
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .unwrap();
    session_registry
        .close_retained_graph_runtimes_for_shutdown()
        .await
        .expect("joined session sync tasks release graph leases before terminal close");
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
    extend_tasks(
        &service,
        [
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
        ],
    );

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
    assert!(
        service
            .context_for(&scope_b)
            .unwrap()
            .project_sessions()
            .unwrap()
            .shares_client_with(&database_b)
    );
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

    // Re-enter the canonical host-admission owner map. This mints a fresh
    // short-lived registered lease without recovering a runtime or authority
    // from the retired client.
    let replacement_runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        root_a.path(),
        root_a.path().join(project_a.as_str()),
        project_a.clone(),
    )
    .await
    .unwrap();
    let replacement_a = replacement_runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let recovery_request = SessionSyncRequestV1::new(
        RequestId::new("session-sync.rebind-recovery").unwrap(),
        IdempotencyKey::new("session-sync.rebind-recovery").unwrap(),
        scope_a.clone(),
        Deadline::new(UtcMicros(1)).unwrap(),
        CancellationSignal::active("session-sync.rebind-recovery").unwrap(),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let recovery_key = journal_key(&scope_a, recovery_request.idempotency_key());
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
    assert!(!old_a.shares_client_with(&replacement_a));
    assert_eq!(old_a.binding(), replacement_a.binding());
    assert!(
        service
            .rebind_project(&profile_a, &project_a, &replacement_a)
            .await
            .unwrap()
    );
    assert!(
        service
            .context_for(&scope_a)
            .unwrap()
            .project_sessions()
            .unwrap()
            .shares_client_with(&replacement_a)
    );
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
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Profile)
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
    let key = journal_key(&scope, request.idempotency_key());
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
                user_sessions: profile_sessions.clone(),
                registry: profile_sessions,
                startup_import: false,
                project_refresh: SessionTemporalRefreshWake::unavailable(),
                user_refresh: SessionTemporalRefreshWake::unavailable(),
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
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Profile)
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
            journal_key(&scope, primary_request.idempotency_key()),
            primary,
        ),
        (journal_key(&scope, alias_request.idempotency_key()), alias),
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
            user_sessions: profile_sessions.clone(),
            registry: profile_sessions.clone(),
            startup_import: true,
            project_refresh: SessionTemporalRefreshWake::unavailable(),
            user_refresh: SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .unwrap();

    let journals = profile_sessions
        .list_session_sync_journals(&journal_prefix(&scope))
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

/// The dogfood defect this recovers from: a profile session store keeps one
/// `source_cursors` row per observed session source, and a long-lived profile
/// exceeds what the `SQLite` runtime materializes for one exact-SQL query.
/// Recovering any live journal refreshes source frontiers during
/// `register_project`, so an unbounded frontier read degraded every project
/// full-upgrade (`project_open_phase` = `full_upgrade_degraded`). Registration
/// must page those reads and succeed on arbitrarily large stores.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recovery_upgrades_a_journal_whose_frontiers_exceed_one_query() {
    const CURSOR_ROWS: i64 = 10_001;
    let service = DaemonSessionSyncService::default();
    let root = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.session-sync.large-frontier").unwrap();
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
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let profile_sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Profile)
        .unwrap();
    let brain_id = project_sessions.binding().shard_id.brain_id.clone();
    let profile_id = project_sessions.binding().shard_id.profile_id.clone();
    let scope = SessionSyncScopeV1::new(project_id.clone(), profile_id.clone());
    profile_sessions
        .writer_connection()
        .unwrap()
        .execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1 UNION ALL SELECT value + 1 FROM fixture WHERE value < {CURSOR_ROWS}
                 )
                 INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 SELECT json_object('session_id', printf('session-%07d', value)),
                        json_object('kind', 'profile'),
                        json_object('position', value)
                 FROM fixture"
            ),
            tracedecay_runtime_core::db::engine::params![],
        )
        .await
        .unwrap();
    let live_request = request(project_id.clone(), profile_id.clone());
    let live_key = journal_key(&scope, live_request.idempotency_key());
    let live = SessionSyncJournalV1::queued(&live_request, UtcMicros(1));
    assert!(
        profile_sessions
            .insert_session_sync_journal(&live_key, &serde_json::to_string(&live).unwrap())
            .await
            .unwrap()
    );

    service
        .register_project(DaemonSessionSyncConfig {
            brain_id,
            profile_id,
            project_id,
            profile_root: root.path().to_path_buf(),
            project_root,
            transcript_source_home: None,
            project_sessions,
            user_sessions: profile_sessions.clone(),
            registry: profile_sessions.clone(),
            startup_import: false,
            project_refresh: SessionTemporalRefreshWake::unavailable(),
            user_refresh: SessionTemporalRefreshWake::unavailable(),
        })
        .await
        .expect("recovery over a store larger than one exact-SQL query must not degrade the mount");

    let refreshed = profile_sessions
        .read_session_sync_journal(&live_key)
        .await
        .unwrap()
        .unwrap();
    let refreshed: SessionSyncJournalV1 = serde_json::from_str(&refreshed).unwrap();
    assert!(
        refreshed.source_frontiers.len() >= usize::try_from(CURSOR_ROWS).unwrap(),
        "the recovered journal must carry the complete paged frontier scan"
    );
    SessionSyncServicePort::shutdown(&service).await;
}

/// `TRACEDECAY_SESSION_INGEST_DISABLED` must stop transcript ingest without
/// unmounting anything else.
///
/// The live failure it covers: with the switch on, the startup import returned
/// `Unavailable { session_ingest_disabled_by_env }`, `schedule_startup_import`
/// reported that as a failed admission, the project's session context was
/// retired, and the whole project full-upgrade failed
/// (`full_upgrade_degraded ... session sync startup import was not admitted`).
/// That same block admits the code-index scheduler, so every later background
/// refresh answered `code_index_scheduler_unavailable` and the daemon could
/// neither seal nor seat a code generation.
#[test]
fn configured_off_transcript_ingest_does_not_fail_the_project_mount() {
    assert!(
        DaemonSessionSyncService::classify_startup_import_outcome(
            SessionSyncOutcomeV1::Unavailable {
                reason_code: SESSION_INGEST_DISABLED_REASON_V1,
            },
        )
        .is_ok(),
        "a configured-off ingest lane is a deliberate no-op and must never roll back the mount"
    );
}

/// The skip above is exactly one reason code wide: every other unadmitted
/// outcome is still a genuine failure that must roll the session context back.
#[test]
fn other_unadmitted_startup_import_outcomes_still_fail_the_project_mount() {
    for outcome in [
        SessionSyncOutcomeV1::Unavailable {
            reason_code: "session_sync_coalesced_journal_read_failed",
        },
        SessionSyncOutcomeV1::Cancelled,
        SessionSyncOutcomeV1::DeadlineExceeded,
        SessionSyncOutcomeV1::WrongScope,
    ] {
        let refused = DaemonSessionSyncService::classify_startup_import_outcome(outcome.clone());
        assert!(
            refused.is_err(),
            "startup import outcome {outcome:?} must still fail the project mount"
        );
    }
}
