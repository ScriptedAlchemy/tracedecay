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
async fn daemon_memory_repair_scheduler_shutdown_aborts_and_joins_every_loop() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/memory-repair-shutdown-test"),
            global_db_path: PathBuf::from("/profiles/memory-repair-shutdown-test/global.db"),
            project_id: Some("memory-repair-shutdown-test".to_string()),
            store_root: PathBuf::from("/stores/memory-repair-shutdown-test"),
            graph_db_path: PathBuf::from("/stores/memory-repair-shutdown-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/memory-repair-shutdown-test"),
        scope_prefix: None,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key, MemoryRepairSchedulerHandle::for_test(task));

    engine.lifecycle.begin_draining();
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.shutdown_memory_repair_schedulers(),
    )
    .await
    .expect("memory-repair shutdown should not wait for its retry delay");

    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
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
#[tokio::test]
async fn repair_shutdown_timeout_keeps_unfinished_task_tracked() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/repair-shutdown-timeout-test"),
            global_db_path: PathBuf::from("/profiles/repair-shutdown-timeout-test/global.db"),
            project_id: Some("repair-shutdown-timeout-test".to_string()),
            store_root: PathBuf::from("/stores/repair-shutdown-timeout-test"),
            graph_db_path: PathBuf::from("/stores/repair-shutdown-timeout-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/repair-shutdown-timeout-test"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("noncooperative repair owner started");
    let stale_task = task.abort_handle();
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key.clone(), MemoryRepairSchedulerHandle::for_test(task));

    engine.lifecycle.begin_draining();
    engine.shutdown_memory_repair_schedulers().await;

    assert!(
        !stale_task.is_finished(),
        "noncooperative repair owner must remain live until released"
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty(),
        "shutdown must transfer repair-map ownership to the tracked reaper"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1,
        "timed-out repair shutdown must retain one tracked join reaper"
    );

    release.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .expect("noncooperative repair owner completion timed out")
        .expect("noncooperative repair owner completed");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if engine
                .store_administration
                .memory_repair_schedulers()
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
    .expect("repair shutdown reaper did not release owner state");
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_contended_repair_retirement_blocks_restart_until_join() {
    use super::super::memory_repair_scheduler::MemoryRepairSchedulerReconcileOutcome;

    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/repair-registration-cancel-test"),
            global_db_path: PathBuf::from("/profiles/repair-registration-cancel-test/global.db"),
            project_id: Some("repair-registration-cancel-test".to_string()),
            store_root: PathBuf::from("/stores/repair-registration-cancel-test"),
            graph_db_path: PathBuf::from("/stores/repair-registration-cancel-test/old.db"),
        },
        project_root: PathBuf::from("/projects/repair-registration-cancel-test"),
        scope_prefix: None,
    };
    let mut replacement = key.clone();
    replacement.owner.graph_db_path =
        PathBuf::from("/stores/repair-registration-cancel-test/new.db");
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, started_rx)
        .await
        .expect("noncooperative repair owner start timed out")
        .expect("noncooperative repair owner started");
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(key.clone(), MemoryRepairSchedulerHandle::for_test(task));
    let barrier = engine
        .store_administration
        .install_retirement_reaper_registration_barrier_for_test();
    let retirement_engine = engine.clone();
    let retirement_key = key.clone();
    let retirement = tokio::spawn(async move {
        retirement_engine
            .retire_memory_repair_scheduler_locked(&retirement_key)
            .await
    });
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, barrier.wait_until_reached())
        .await
        .expect("repair registration barrier was not reached");

    retirement.abort();
    barrier.release();
    let _ = tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, retirement)
        .await
        .expect("cancelled repair retirement did not unwind");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(1),
    )
    .await
    .expect("repair reaper was not registered after caller cancellation");
    let repeated = engine
        .retire_memory_repair_scheduler_locked(&key)
        .await
        .expect("repeated repair retirement must reuse the tombstone");
    assert_eq!(
        engine.store_administration.retirement_reaper_count().await,
        1
    );
    assert_eq!(
        engine
            .ensure_memory_repair_scheduler(
                replacement,
                PathBuf::from("/moved-project"),
                test_handshake_defaults(),
            )
            .await,
        MemoryRepairSchedulerReconcileOutcome::Retiring,
        "repair restart must remain blocked until the old task joins"
    );

    release.release();
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, completed_rx)
        .await
        .expect("repair owner completion timed out")
        .expect("repair owner completion sender dropped");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, repeated.wait())
        .await
        .expect("repeated repair retirement did not complete");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("repair reaper ownership did not converge to zero");
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn panicked_retired_tasks_release_both_scheduler_registrations() {
    let engine = DaemonEngine::default();
    let automation_key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/panicked-automation-retirement-test"),
            global_db_path: PathBuf::from(
                "/profiles/panicked-automation-retirement-test/global.db",
            ),
            project_id: Some("panicked-automation-retirement-test".to_string()),
            store_root: PathBuf::from("/stores/panicked-automation-retirement-test"),
            graph_db_path: PathBuf::from("/stores/panicked-automation-retirement-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/panicked-automation-retirement-test"),
        scope_prefix: None,
    };
    let repair_key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/panicked-repair-retirement-test"),
            global_db_path: PathBuf::from("/profiles/panicked-repair-retirement-test/global.db"),
            project_id: Some("panicked-repair-retirement-test".to_string()),
            store_root: PathBuf::from("/stores/panicked-repair-retirement-test"),
            graph_db_path: PathBuf::from("/stores/panicked-repair-retirement-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/panicked-repair-retirement-test"),
        scope_prefix: None,
    };
    let automation_task = tokio::spawn(async {
        panic!("panicked automation owner");
    });
    let repair_task = tokio::spawn(async {
        panic!("panicked repair owner");
    });
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(
            automation_key.clone(),
            test_automation_scheduler_handle(automation_task),
        );
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(
            repair_key.clone(),
            MemoryRepairSchedulerHandle::for_test(repair_task),
        );

    let automation_retirement = engine
        .retire_automation_scheduler_locked(&automation_key)
        .await
        .expect("panicked automation retirement");
    let repair_retirement = engine
        .retire_memory_repair_scheduler_locked(&repair_key)
        .await
        .expect("panicked repair retirement");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, async {
        automation_retirement.wait().await;
        repair_retirement.wait().await;
    })
    .await
    .expect("panicked scheduler retirements did not complete");
    tokio::time::timeout(
        MAINTENANCE_TEST_DEADLINE,
        engine
            .store_administration
            .wait_for_retirement_reaper_count_for_test(0),
    )
    .await
    .expect("panicked task reapers did not converge to zero");
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
    assert!(
        engine
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn scheduler_shutdown_does_not_wait_for_contended_administration_gate() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/shutdown-gate-test"),
            global_db_path: PathBuf::from("/profiles/shutdown-gate-test/global.db"),
            project_id: Some("shutdown-gate-test".to_string()),
            store_root: PathBuf::from("/stores/shutdown-gate-test"),
            graph_db_path: PathBuf::from("/stores/shutdown-gate-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/shutdown-gate-test"),
        scope_prefix: None,
    };
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(
            key.clone(),
            test_automation_scheduler_handle(tokio::spawn(std::future::pending::<()>())),
        );
    engine
        .store_administration
        .memory_repair_schedulers()
        .lock()
        .await
        .insert(
            key,
            MemoryRepairSchedulerHandle::for_test(tokio::spawn(std::future::pending::<()>())),
        );
    let (gate_entered_tx, gate_entered_rx) = tokio::sync::oneshot::channel();
    let (gate_release_tx, gate_release_rx) = tokio::sync::oneshot::channel();
    let administration = engine.store_administration.clone();
    let gate_holder = tokio::spawn(async move {
        administration
            .with_writer(|| async move {
                let _ = gate_entered_tx.send(());
                let _ = gate_release_rx.await;
            })
            .await;
    });
    gate_entered_rx
        .await
        .expect("administration gate holder started");

    engine.lifecycle.begin_draining();
    let shutdown_engine = engine.clone();
    let mut shutdown = tokio::spawn(async move {
        tokio::join!(
            shutdown_engine.shutdown_automation_schedulers(),
            shutdown_engine.shutdown_memory_repair_schedulers(),
        );
    });
    let completed_without_gate =
        tokio::time::timeout(std::time::Duration::from_millis(250), &mut shutdown)
            .await
            .is_ok();
    let _ = gate_release_tx.send(());
    gate_holder.await.expect("administration gate holder exits");
    if !completed_without_gate {
        shutdown
            .await
            .expect("scheduler shutdown exits after gate release");
    }

    assert!(
        completed_without_gate,
        "normal scheduler shutdown must not queue behind unrelated writer administration"
    );
}
