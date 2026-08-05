#[cfg(unix)]
use super::*;

#[cfg(unix)]
const MAINTENANCE_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(unix)]
#[tokio::test]
async fn daemon_scheduler_shutdown_aborts_and_joins_every_loop() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/shutdown-test"),
            global_db_path: PathBuf::from("/profiles/shutdown-test/global.db"),
            project_id: Some("shutdown-test".to_string()),
            store_root: PathBuf::from("/stores/shutdown-test"),
            graph_db_path: PathBuf::from("/stores/shutdown-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/shutdown-test"),
        scope_prefix: None,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key, test_automation_scheduler_handle(task));

    engine.lifecycle.begin_draining();
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.shutdown_automation_schedulers(),
    )
    .await
    .expect("scheduler shutdown should not wait for its tick interval");

    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn automation_shutdown_timeout_keeps_unfinished_task_tracked() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/automation-shutdown-timeout-test"),
            global_db_path: PathBuf::from("/profiles/automation-shutdown-timeout-test/global.db"),
            project_id: Some("automation-shutdown-timeout-test".to_string()),
            store_root: PathBuf::from("/stores/automation-shutdown-timeout-test"),
            graph_db_path: PathBuf::from("/stores/automation-shutdown-timeout-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/automation-shutdown-timeout-test"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative automation owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));

    engine.lifecycle.begin_draining();
    engine.shutdown_automation_schedulers().await;

    assert!(
        !stale_task.is_finished(),
        "noncooperative automation owner must remain live until released"
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must transfer scheduler-map ownership to the tracked reaper"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "timed-out automation shutdown must retain one tracked join reaper"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("noncooperative automation owner completion timed out")
        .expect("noncooperative automation owner completed");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if engine
                .store_administration
                .automation_schedulers()
                .lock()
                .await
                .is_empty()
                && engine.store_administration.retirement_reaper_count().await == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automation shutdown reaper did not release owner state");
    assert!(stale_task.is_finished());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_contended_automation_retirement_remains_shutdown_owned() {
    use crate::dashboard::AutomationSchedulerReconcileOutcome;

    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/automation-registration-cancel-test"),
            global_db_path: PathBuf::from(
                "/profiles/automation-registration-cancel-test/global.db",
            ),
            project_id: Some("automation-registration-cancel-test".to_string()),
            store_root: PathBuf::from("/stores/automation-registration-cancel-test"),
            graph_db_path: PathBuf::from("/stores/automation-registration-cancel-test/old.db"),
        },
        project_root: PathBuf::from("/projects/automation-registration-cancel-test"),
        scope_prefix: None,
    };
    let mut replacement = key.clone();
    replacement.owner.graph_db_path =
        PathBuf::from("/stores/automation-registration-cancel-test/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, started_rx)
        .await
        .expect("noncooperative automation owner start timed out")
        .expect("noncooperative automation owner started");
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));
    let barrier = engine
        .store_administration
        .install_retirement_reaper_registration_barrier_for_test();
    let retirement_engine = engine.clone();
    let retirement_key = key.clone();
    let retirement = tokio::spawn(async move {
        retirement_engine
            .retire_automation_scheduler_locked(&retirement_key)
            .await
    });
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, barrier.wait_until_reached())
        .await
        .expect("automation registration barrier was not reached");

    retirement.abort();
    barrier.release();
    let _ = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retirement)
        .await
        .expect("cancelled automation retirement did not unwind");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(1),
    )
    .await
    .expect("automation reaper was not registered after caller cancellation");
    let repeated = engine
        .retire_automation_scheduler_locked(&key)
        .await
        .expect("repeated automation retirement must reuse the tombstone");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "repeated retirement must not add a second reaper"
    );
    assert_eq!(
        engine
            .ensure_automation_scheduler(
                replacement,
                PathBuf::from("/moved-project"),
                test_handshake_defaults(),
            )
            .await,
        AutomationSchedulerReconcileOutcome::Retiring,
        "restart must remain blocked while the old task is live"
    );

    let first_pass = engine
        .store_administration
        .retirement_reaper_shutdown_passes_for_test();
    let shutdown_administration = engine.store_administration.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_administration.shutdown_retirement_reapers().await;
    });
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_shutdown_pass_for_test(first_pass),
    )
    .await
    .expect("reaper shutdown did not observe registered automation ownership");
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the noncooperative automation owner"
    );
    shutdown.abort();
    let shutdown_result = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, shutdown)
        .await
        .expect("cancelled reaper shutdown did not unwind");
    assert!(
        matches!(shutdown_result, Err(error) if error.is_cancelled()),
        "the first reaper shutdown must be cancelled at its wait point"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "cancelled shutdown must leave registry ownership intact"
    );

    let retry_pass = engine
        .store_administration
        .retirement_reaper_shutdown_passes_for_test();
    let retry_administration = engine.store_administration.clone();
    let retry = tokio::spawn(async move {
        retry_administration.shutdown_retirement_reapers().await;
    });
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_shutdown_pass_for_test(retry_pass),
    )
    .await
    .expect("repeated reaper shutdown did not rediscover automation ownership");
    assert!(!retry.is_finished());

    release.release();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, completed_rx)
        .await
        .expect("automation owner completion timed out")
        .expect("automation owner completion sender dropped");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retry)
        .await
        .expect("repeated reaper shutdown timed out")
        .expect("repeated reaper shutdown panicked");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, repeated.wait())
        .await
        .expect("repeated automation retirement did not complete");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        0
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine.store_administration.shutdown_retirement_reapers(),
    )
    .await
    .expect("idempotent reaper shutdown timed out");
}
