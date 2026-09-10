#[cfg(unix)]
use super::*;

#[cfg(unix)]
const MAINTENANCE_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(unix)]
#[tokio::test]
async fn pr_autotrack_is_cancelled_before_invocation_join() {
    let engine = DaemonEngine::default();
    let task = crate::daemon::pr_autotrack::spawn_with_administration(
        engine.store_administration.clone(),
        tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(1),
    );
    let cancellation = task.cancellation();
    let engine = engine.with_pr_autotrack_task(task).await;

    let owner_phases = engine.shutdown_owner_phases().await;
    assert!(!cancellation.is_cancelled());

    let prepared =
        crate::daemon::shutdown_coordination::prepare_shutdown_owner_phases(owner_phases);
    assert!(
        cancellation.is_cancelled(),
        "PR auto-track must be cancelled before invocation join begins"
    );

    let _ = prepared
        .join(tokio::time::Instant::now() + std::time::Duration::from_secs(1))
        .await;
}

#[cfg(unix)]
#[tokio::test]
async fn manual_branch_add_journey_is_joined_by_daemon_shutdown() {
    let engine = DaemonEngine::default();
    let administration = engine.store_administration.clone();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let admission = administration
        .admit_manual_branch_publication(|_, admitted| async move {
            let _ = admitted.send(());
            let _ = started_sender.send(());
            let _ = release_receiver.await;
            Ok(tracedecay_runtime_core::branch::BranchAddOutcome::Added)
        })
        .await
        .expect("manual branch publication is admitted");
    assert_eq!(
        admission,
        tracedecay_runtime_core::branch::BranchAddOutcome::Deferred,
        "branch add must return while exact publication continues"
    );
    started_receiver
        .await
        .expect("manual branch publication starts");

    let mut owner_phases = engine.shutdown_owner_phases().await;
    let manual_branch_phase = owner_phases.remove(0);
    let prepared = crate::daemon::shutdown_coordination::prepare_shutdown_owner_phases(vec![
        manual_branch_phase,
    ]);
    let denied = engine
        .store_administration
        .run_manual_branch_publication(|_| async {
            Ok(tracedecay_runtime_core::branch::BranchAddOutcome::AlreadyTracked)
        })
        .await
        .expect_err("shutdown closes manual branch publication admission");
    assert_eq!(
        denied.project_route_context().map(|(reason, _, _)| reason),
        Some("branch_tracking_failed")
    );

    let shutdown = tokio::spawn(async move {
        prepared
            .join(tokio::time::Instant::now() + std::time::Duration::from_secs(1))
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !shutdown.is_finished(),
        "daemon shutdown must retain the active manual branch publication"
    );

    let _ = release_sender.send(());
    assert_eq!(
        request
            .await
            .expect("manual branch request task joins")
            .expect("manual branch publication succeeds"),
        tracedecay_runtime_core::branch::BranchAddOutcome::Added
    );
    let receipt = shutdown.await.expect("daemon shutdown task joins");
    assert!(
        receipt.unfinished().is_empty(),
        "manual branch publication shutdown must complete cleanly"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn stalled_manual_branch_publication_settles_before_shutdown_receipt() {
    let engine = DaemonEngine::default();
    let administration = engine.store_administration.clone();
    let mutation_after_terminal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mutation = std::sync::Arc::clone(&mutation_after_terminal);
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let request = tokio::spawn(async move {
        administration
            .run_manual_branch_publication(
                move |cancellation: tracedecay_runtime_core::cancellation::CancellationToken| async move {
                let _ = started_sender.send(());
                tokio::select! {
                    () = cancellation.cancelled() => {
                        Err(tracedecay_domain::errors::TraceDecayError::project_route(
                            "branch_tracking_failed",
                            true,
                            "manual branch publication cancelled by daemon shutdown",
                        ))
                    }
                    () = tokio::time::sleep(std::time::Duration::from_mins(1)) => {
                        mutation.store(true, std::sync::atomic::Ordering::Release);
                        Ok(tracedecay_runtime_core::branch::BranchAddOutcome::Added)
                    }
                }
            },
            )
            .await
    });
    started_receiver
        .await
        .expect("manual branch publication starts");

    let prepared = crate::daemon::shutdown_coordination::prepare_shutdown_owner_phases(
        engine.shutdown_owner_phases().await,
    );
    let shutdown = tokio::spawn(async move {
        prepared
            .join(tokio::time::Instant::now() + std::time::Duration::from_secs(15))
            .await
    });
    tokio::time::advance(std::time::Duration::from_secs(16)).await;
    let receipt = shutdown.await.expect("shutdown joins");
    assert!(
        receipt.unfinished().is_empty(),
        "cooperative cancellation must settle before the terminal receipt"
    );
    assert!(
        request.await.expect("publication request joins").is_err(),
        "cancelled publication must report failure"
    );
    tokio::time::advance(std::time::Duration::from_mins(1)).await;
    assert!(
        !mutation_after_terminal.load(std::sync::atomic::Ordering::Acquire),
        "manual publication mutated state after shutdown terminal receipt"
    );
}

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
    engine.cancel_automation_schedulers();
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
        engine.store_administration.retirement_reaper_count(),
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
                && engine.store_administration.retirement_reaper_count() == 0
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_reaper_shutdown_observes_a_reservation_release_after_snapshot() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/reaper-release-race"),
            global_db_path: PathBuf::from("/profiles/reaper-release-race/global.db"),
            project_id: Some("reaper-release-race".to_owned()),
            store_root: PathBuf::from("/stores/reaper-release-race"),
            graph_db_path: PathBuf::from("/stores/reaper-release-race/graph.db"),
        },
        project_root: PathBuf::from("/projects/reaper-release-race"),
        scope_prefix: None,
    };
    let reservation = engine
        .store_administration
        .reserve_retirement_reaper(&key)
        .expect("reserve retirement handoff");
    let previous_pass = engine
        .store_administration
        .retirement_reaper_shutdown_passes_for_test();
    let shutdown_administration = engine.store_administration.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_administration.shutdown_retirement_reapers().await;
    });
    engine
        .store_administration
        .wait_for_retirement_reaper_shutdown_pass_for_test(previous_pass)
        .await;

    drop(reservation);
    tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
        .await
        .expect("reservation release must wake terminal reaper shutdown")
        .expect("retirement reaper shutdown task");
    assert_eq!(
        engine.store_administration.retirement_reaper_counts(),
        (0, 0)
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automation_retirement_reserves_only_after_scheduler_lock_admission() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/retirement-lock-admission"),
            global_db_path: PathBuf::from("/profiles/retirement-lock-admission/global.db"),
            project_id: Some("retirement-lock-admission".to_owned()),
            store_root: PathBuf::from("/stores/retirement-lock-admission"),
            graph_db_path: PathBuf::from("/stores/retirement-lock-admission/graph.db"),
        },
        project_root: PathBuf::from("/projects/retirement-lock-admission"),
        scope_prefix: None,
    };
    let task = tokio::spawn(std::future::pending::<()>());
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));
    let schedulers = engine.store_administration.automation_schedulers().clone();
    let lock = schedulers.lock().await;
    let mut retirement = Box::pin(engine.retire_automation_scheduler_locked(&key));

    assert!(
        matches!(
            futures_util::poll!(&mut retirement),
            std::task::Poll::Pending
        ),
        "retirement must wait behind the scheduler mutex"
    );
    assert_eq!(
        engine.store_administration.retirement_reaper_counts(),
        (0, 0),
        "mutex contention must not publish a pending retirement handoff"
    );

    drop(lock);
    let retirement = retirement.await.expect("retirement owner");
    tokio::time::timeout(std::time::Duration::from_secs(1), retirement.wait())
        .await
        .expect("retirement reaper completion");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        engine.store_administration.shutdown_retirement_reapers(),
    )
    .await
    .expect("terminal reaper shutdown");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_contended_automation_retirement_remains_shutdown_owned() {
    use tracedecay_dashboard_api::AutomationSchedulerReconcileOutcome;

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
        engine.store_administration.retirement_reaper_count(),
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
        engine.store_administration.retirement_reaper_count(),
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
    assert_eq!(engine.store_administration.retirement_reaper_count(), 0);
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
async fn deletion_drain_retains_already_transferred_maintenance_reaper_until_retry() {
    let engine = DaemonEngine::default();
    let key = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/deletion-reaper-test"),
            global_db_path: PathBuf::from("/profiles/deletion-reaper-test/global.db"),
            project_id: Some("deletion-reaper-test".to_owned()),
            store_root: PathBuf::from("/stores/deletion-reaper-test"),
            graph_db_path: PathBuf::from("/stores/deletion-reaper-test/graph.db"),
        },
        project_root: PathBuf::from("/projects/deletion-reaper-test"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx.await.expect("maintenance owner started");
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(key.clone(), test_automation_scheduler_handle(task));
    let retirement = engine
        .retire_automation_scheduler_locked(&key)
        .await
        .expect("transfer owner to retirement reaper");
    engine
        .store_administration
        .wait_for_retirement_reaper_count_for_test(1)
        .await;

    assert!(
        !engine
            .store_administration
            .settle_retirement_reapers(tokio::time::Duration::from_millis(25))
            .await,
        "deletion must remain settling while a transferred owner can still write"
    );
    assert_eq!(engine.store_administration.retirement_reaper_count(), 1);
    release.release();
    completed_rx.await.expect("maintenance owner completed");
    assert!(
        engine
            .store_administration
            .settle_retirement_reapers(tokio::time::Duration::from_secs(1))
            .await,
        "retry must join the retained retirement reaper"
    );
    retirement.wait().await;
    assert_eq!(engine.store_administration.retirement_reaper_count(), 0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_reaper_settle_ignores_unrelated_pending_registration() {
    let engine = DaemonEngine::default();
    let unrelated = ProjectServerKey {
        owner: StoreOwnerKey {
            profile_root: PathBuf::from("/profiles/unrelated-pending-reaper"),
            global_db_path: PathBuf::from("/profiles/unrelated-pending-reaper/global.db"),
            project_id: Some("proj_unrelated_pending_reaper".to_owned()),
            store_root: PathBuf::from("/stores/unrelated-pending-reaper"),
            graph_db_path: PathBuf::from("/stores/unrelated-pending-reaper/graph.db"),
        },
        project_root: PathBuf::from("/projects/unrelated-pending-reaper"),
        scope_prefix: None,
    };
    let (task, started_rx, completed_rx, release) = spawn_noncooperative_test_task();
    started_rx
        .await
        .expect("unrelated maintenance owner started");
    engine
        .store_administration
        .automation_schedulers()
        .lock()
        .await
        .insert(unrelated.clone(), test_automation_scheduler_handle(task));
    let barrier = engine
        .store_administration
        .install_retirement_reaper_registration_barrier_for_test();
    let retirement_engine = engine.clone();
    let retirement_key = unrelated.clone();
    let retirement = tokio::spawn(async move {
        retirement_engine
            .retire_automation_scheduler_locked(&retirement_key)
            .await
    });
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, barrier.wait_until_reached())
        .await
        .expect("unrelated reaper registration barrier was not reached");

    let target_settled = engine
        .store_administration
        .settle_retirement_reapers_for_project(
            std::path::Path::new("/profiles/target-project"),
            "proj_target",
            tokio::time::Duration::from_millis(25),
        )
        .await;
    barrier.release();
    let retirement = retirement
        .await
        .expect("unrelated retirement task")
        .expect("unrelated retirement ownership");
    release.release();
    completed_rx
        .await
        .expect("unrelated maintenance owner completed");
    retirement.wait().await;
    assert!(
        target_settled,
        "target cleanup must not wait on an unrelated pending reaper registration"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn panicked_retired_task_releases_scheduler_registration() {
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
    let automation_task = tokio::spawn(async {
        panic!("panicked automation owner");
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
    let automation_retirement = engine
        .retire_automation_scheduler_locked(&automation_key)
        .await
        .expect("panicked automation retirement");
    tokio::time::timeout(MAINTENANCE_TEST_DEADLINE, automation_retirement.wait())
        .await
        .expect("panicked scheduler retirement did not complete");
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
            key,
            test_automation_scheduler_handle(tokio::spawn(std::future::pending::<()>())),
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
        shutdown_engine.shutdown_automation_schedulers().await;
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

#[cfg(unix)]
#[tokio::test]
async fn manual_branch_publication_panic_survives_reaping_in_shutdown_receipt() {
    let engine = DaemonEngine::default();
    let failure = engine
        .store_administration
        .run_manual_branch_publication(|_| async { panic!("publication owner failed") })
        .await;
    assert!(failure.is_err());
    engine
        .store_administration
        .run_manual_branch_publication(|_| async {
            Ok(tracedecay_runtime_core::branch::BranchAddOutcome::AlreadyTracked)
        })
        .await
        .unwrap();
    let mut phases = engine.shutdown_owner_phases().await;
    let receipt =
        crate::daemon::shutdown_coordination::prepare_shutdown_owner_phases(vec![phases.remove(0)])
            .join(tokio::time::Instant::now() + std::time::Duration::from_secs(1))
            .await;
    assert!(
        matches!(&receipt.owners[0].status, crate::daemon::shutdown_coordination::ShutdownStatus::Failed(reason) if reason.contains("failed to join"))
    );
}

/// The dogfood supervisor gives the daemon a 5 s TERM grace. A background
/// code-index reconcile that is still sealing when shutdown starts must be
/// cancelled through its fence and joined inside that grace, not abandoned
/// at the task-abort deadline and then waited for again at runtime teardown.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_shutdown_with_code_index_reconcile_in_flight_stays_inside_term_grace() {
    let home = TempDir::new().expect("isolated home");
    let home = home.path().canonicalize().expect("canonical home");
    let repository = home.join("repository");
    std::fs::create_dir_all(repository.join("src")).expect("create repository");
    super::bootstrap::run_git(&repository, &["init", "-b", "main", "--quiet"]);
    for file in 0..24 {
        std::fs::write(
            repository.join(format!("src/module_{file}.rs")),
            format!("pub fn sealed_{file}() -> u32 {{ {file} }}\n"),
        )
        .expect("fixture source");
    }
    super::bootstrap::run_git(&repository, &["add", "."]);
    super::bootstrap::run_git(&repository, &["commit", "-m", "fixture", "--quiet"]);

    let handshake = DaemonHandshake {
        project_path: Some(repository.clone()),
        allow_init: true,
        client_identity: test_client_identity_for(home.join("client")),
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "shutdown-term-grace-test",
    );
    let server = engine
        .project_server(&handshake)
        .await
        .expect("project open must publish a server");
    let project_id = server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("registered project identity");
    let project_id = tracedecay_domain::ProjectId::new(project_id).expect("project id");
    let canonical_repository = repository.canonicalize().expect("canonical repository");
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        &canonical_repository,
        &project_id,
    )
    .expect("code-index scope");
    // Demand-driven activation mounts the background worker and wakes its
    // first reconcile pass; shutdown follows without waiting for it.
    let _warming = engine
        .invocation
        .code_index_schedulers
        .latest_complete_ready_for_scope(&scope)
        .await;

    let started = std::time::Instant::now();
    let receipt = engine.shutdown_all().await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "engine shutdown must finish inside the 5 s TERM grace, took {elapsed:?}"
    );
    assert!(
        receipt.background.unfinished().is_empty(),
        "every background owner must join cleanly: {:?}",
        receipt.background.unfinished()
    );
}
