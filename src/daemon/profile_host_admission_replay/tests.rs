use super::*;
use std::sync::atomic::AtomicUsize;

#[test]
fn profile_replay_backoff_grows_then_caps() {
    assert_eq!(profile_replay_backoff(1), Duration::from_millis(25));
    assert_eq!(profile_replay_backoff(2), Duration::from_millis(50));
    assert_eq!(profile_replay_backoff(3), Duration::from_millis(100));
    assert_eq!(profile_replay_backoff(20), Duration::from_secs(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_bootstrap_ensures_coalesce_and_cache_readiness() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry = Arc::new(ProfileHostAdmissionReplayRegistry::default());
    let attempts = Arc::new(AtomicUsize::new(0));
    let operation_attempts = Arc::clone(&attempts);
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let attempts = Arc::clone(&operation_attempts);
        Box::pin(async move {
            attempts.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok(())
        })
    });

    let mut tasks = JoinSet::new();
    for _ in 0..8 {
        let registry = Arc::clone(&registry);
        let profile_root = profile_root.clone();
        let operation = Arc::clone(&operation);
        tasks.spawn(async move {
            registry.ensure_bootstrap(&profile_root, operation).await;
        });
    }
    while tasks.join_next().await.is_some() {}
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(2))
            .await,
        "coalesced bootstrap must complete"
    );
    assert_eq!(attempts.load(Ordering::Acquire), 1);
    assert_eq!(registry.bootstrap_worker_count().await, 1);

    registry
        .ensure_bootstrap(&profile_root, Arc::clone(&operation))
        .await;
    assert_eq!(registry.bootstrap_attempt_count(&profile_root).await, 1);
    assert_eq!(attempts.load(Ordering::Acquire), 1);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_bootstrap_cache_expires_and_revalidates() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry = ProfileHostAdmissionReplayRegistry::with_bootstrap_cache_for(
        Duration::from_millis(20),
        Duration::from_millis(20),
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let operation_attempts = Arc::clone(&attempts);
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let attempts = Arc::clone(&operation_attempts);
        Box::pin(async move {
            attempts.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    });

    registry
        .ensure_bootstrap(&profile_root, Arc::clone(&operation))
        .await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(1))
            .await
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    registry.ensure_bootstrap(&profile_root, operation).await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(1))
            .await
    );
    assert_eq!(attempts.load(Ordering::Acquire), 2);
    assert_eq!(registry.bootstrap_worker_count().await, 1);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_bootstrap_cache_allows_later_repair() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry = ProfileHostAdmissionReplayRegistry::with_bootstrap_cache_for(
        Duration::from_secs(1),
        Duration::from_millis(20),
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let operation_attempts = Arc::clone(&attempts);
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let attempts = Arc::clone(&operation_attempts);
        Box::pin(async move {
            attempts.fetch_add(1, Ordering::AcqRel);
            Err(crate::errors::TraceDecayError::project_route(
                "test_bootstrap_terminal",
                false,
                "repair required",
            ))
        })
    });

    registry
        .ensure_bootstrap(&profile_root, Arc::clone(&operation))
        .await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(1))
            .await
    );
    registry
        .ensure_bootstrap(&profile_root, Arc::clone(&operation))
        .await;
    assert_eq!(attempts.load(Ordering::Acquire), 1);

    tokio::time::sleep(Duration::from_millis(30)).await;
    registry.ensure_bootstrap(&profile_root, operation).await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(1))
            .await
    );
    assert_eq!(attempts.load(Ordering::Acquire), 2);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_retries_transient_failure_without_another_ensure() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let attempts = Arc::new(AtomicUsize::new(0));
    let operation_attempts = Arc::clone(&attempts);
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let attempts = Arc::clone(&operation_attempts);
        Box::pin(async move {
            let attempt = attempts.fetch_add(1, Ordering::AcqRel);
            if attempt < 2 {
                Err(crate::errors::TraceDecayError::project_route(
                    "test_bootstrap_unavailable",
                    true,
                    "transient test failure",
                ))
            } else {
                Ok(())
            }
        })
    });

    registry.ensure_bootstrap(&profile_root, operation).await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(2))
            .await,
        "retrying bootstrap must recover without another request"
    );
    assert_eq!(attempts.load(Ordering::Acquire), 3);
    assert_eq!(registry.bootstrap_attempt_count(&profile_root).await, 3);
    assert_eq!(registry.bootstrap_backoff_count(&profile_root).await, 2);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_gives_up_terminally_once_its_retry_budget_is_spent() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry =
        ProfileHostAdmissionReplayRegistry::with_bootstrap_retry_budget(Duration::from_millis(60));
    let attempts = Arc::new(AtomicUsize::new(0));
    let operation_attempts = Arc::clone(&attempts);
    // Always retryable: without a budget this loops for the daemon's life.
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let attempts = Arc::clone(&operation_attempts);
        Box::pin(async move {
            attempts.fetch_add(1, Ordering::AcqRel);
            Err(crate::errors::TraceDecayError::project_route(
                "test_bootstrap_unavailable",
                true,
                "permanently retryable test failure",
            ))
        })
    });

    registry.ensure_bootstrap(&profile_root, operation).await;
    assert!(
        registry
            .wait_bootstrap_completed(&profile_root, Duration::from_secs(5))
            .await,
        "a permanently retryable bootstrap must still terminate"
    );
    assert_eq!(
        registry.bootstrap_state(&profile_root).await,
        Some(BOOTSTRAP_TERMINAL),
        "spending the retry budget is a terminal give-up, not a success"
    );
    let observed = attempts.load(Ordering::Acquire);
    assert!(observed >= 2, "the budget must allow real retries first");
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        attempts.load(Ordering::Acquire),
        observed,
        "a terminal worker must stop retrying entirely"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_shutdown_cancels_and_joins_in_flight_operation() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let started = Arc::new(Notify::new());
    let operation_started = Arc::clone(&started);
    let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
        let started = Arc::clone(&operation_started);
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<crate::errors::Result<()>>().await
        })
    });

    registry.ensure_bootstrap(&profile_root, operation).await;
    started.notified().await;
    tokio::time::timeout(Duration::from_secs(1), registry.shutdown())
        .await
        .expect("shutdown must cancel and join bootstrap workers");
    assert_eq!(registry.bootstrap_worker_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_ensures_coalesce_to_one_pass() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let passes = Arc::new(AtomicUsize::new(0));
    let passes_for_override = Arc::clone(&passes);
    let pass_override: Arc<
        dyn Fn()
                -> std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
            + Send
            + Sync,
    > = Arc::new(move || {
        let passes = Arc::clone(&passes_for_override);
        Box::pin(async move {
            passes.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(Duration::from_millis(40)).await;
            HostAdmissionOutcome::accepted_for_replay()
        })
    });

    registry
        .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
        .await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    let mut kick_tasks = JoinSet::new();
    for _ in 0..8 {
        let registry_workers = {
            let workers = registry.workers.lock().await;
            Arc::clone(&workers.get(&db_path).expect("worker").worker)
        };
        kick_tasks.spawn(async move {
            registry_workers.kick();
        });
    }
    while kick_tasks.join_next().await.is_some() {}
    assert!(
        registry.wait_idle(&db_path, Duration::from_secs(2)).await,
        "coalesced worker must become idle"
    );
    let observed = registry.pass_count(&db_path).await;
    assert!(
        observed <= 3,
        "simultaneous kicks must coalesce; observed {observed} passes"
    );
    assert!(observed >= 1);
    assert_eq!(passes.load(Ordering::Acquire), observed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retryable_failures_apply_bounded_backoff() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_override = Arc::clone(&attempts);
    let pass_override: Arc<
        dyn Fn()
                -> std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
            + Send
            + Sync,
    > = Arc::new(move || {
        let attempts = Arc::clone(&attempts_for_override);
        Box::pin(async move {
            let n = attempts.fetch_add(1, Ordering::AcqRel);
            if n < 2 {
                HostAdmissionOutcome::retained_unavailable("test_retryable")
            } else {
                HostAdmissionOutcome::accepted_for_replay()
            }
        })
    });

    let started = tokio::time::Instant::now();
    registry
        .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
        .await;
    assert!(
        registry.wait_idle(&db_path, Duration::from_secs(2)).await,
        "retryable worker must become idle after success"
    );
    let elapsed = started.elapsed();
    assert!(
        registry.backoff_count(&db_path).await >= 2,
        "retryable outcomes must count backoff sleeps"
    );
    assert!(
        elapsed >= Duration::from_millis(25 + 50),
        "retryable backoff must delay at least the first two intervals; elapsed={elapsed:?}"
    );
    assert_eq!(attempts.load(Ordering::Acquire), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_no_progress_passes_apply_bounded_backoff() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    broker.admit("test:pending", b"pending").await.unwrap();
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let pass_override = Arc::new(|| {
        Box::pin(async { HostAdmissionOutcome::accepted_for_replay() })
            as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
    });

    registry
        .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
        .await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.backoff_count(&db_path).await < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending no-progress replay must back off instead of spinning");
    assert!(registry.pass_count(&db_path).await <= 3);
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_and_joins_an_in_flight_pass() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let started = Arc::new(Notify::new());
    let started_for_override = Arc::clone(&started);
    let pass_override = Arc::new(move || {
        let started = Arc::clone(&started_for_override);
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<HostAdmissionOutcome>().await
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
    });

    registry
        .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
        .await;
    started.notified().await;
    tokio::time::timeout(Duration::from_secs(1), registry.shutdown())
        .await
        .expect("shutdown must cancel and join replay workers");

    assert_eq!(registry.worker_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_probe_deadline_and_shutdown_are_cancellable() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let probe_started = Arc::new(Notify::new());
    let override_started = Arc::clone(&probe_started);
    let pending_count_override: PendingReplayCountOverride = Arc::new(move || {
        let started = Arc::clone(&override_started);
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<usize>().await
        })
    });
    let worker = Arc::new(ProfileHostAdmissionReplayWorker::new(
        &broker,
        &profile_root,
        Arc::clone(&registry.cancellation),
        None,
        Some(pending_count_override),
    ));
    let task_worker = Arc::clone(&worker);
    let task = tokio::spawn(async move {
        task_worker.run(Duration::from_secs(30)).await;
    });
    registry.workers.lock().await.insert(
        db_path.clone(),
        ReplayWorkerEntry {
            worker: Arc::clone(&worker),
            task,
        },
    );
    probe_started.notified().await;

    let started = Instant::now();
    assert!(
        !registry
            .wait_idle(&db_path, Duration::from_millis(20))
            .await,
        "blocked pending probe must respect the caller's replay grace"
    );
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "blocked pending probe exceeded its bounded grace"
    );
    tokio::time::timeout(Duration::from_secs(1), registry.shutdown())
        .await
        .expect("shutdown must cancel and join a blocked pending probe");
    assert_eq!(registry.worker_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_notification_during_probe_is_not_lost() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry = Arc::new(ProfileHostAdmissionReplayRegistry::default());
    let probe_started = Arc::new(Notify::new());
    let release_probe = Arc::new(Notify::new());
    let probe_calls = Arc::new(AtomicUsize::new(0));
    let pending_count_override: PendingReplayCountOverride = {
        let probe_started = Arc::clone(&probe_started);
        let release_probe = Arc::clone(&release_probe);
        let probe_calls = Arc::clone(&probe_calls);
        Arc::new(move || {
            let probe_started = Arc::clone(&probe_started);
            let release_probe = Arc::clone(&release_probe);
            let probe_calls = Arc::clone(&probe_calls);
            Box::pin(async move {
                if probe_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                    probe_started.notify_one();
                    release_probe.notified().await;
                    1
                } else {
                    0
                }
            })
        })
    };
    let worker = Arc::new(ProfileHostAdmissionReplayWorker::new(
        &broker,
        &profile_root,
        Arc::clone(&registry.cancellation),
        None,
        Some(pending_count_override),
    ));
    let cancellation = Arc::clone(&registry.cancellation);
    let task = tokio::spawn(async move {
        cancellation.wait().await;
    });
    registry.workers.lock().await.insert(
        db_path.clone(),
        ReplayWorkerEntry {
            worker: Arc::clone(&worker),
            task,
        },
    );

    let wait_registry = Arc::clone(&registry);
    let wait_db_path = db_path.clone();
    let wait = tokio::spawn(async move {
        wait_registry
            .wait_idle(&wait_db_path, Duration::from_secs(1))
            .await
    });
    probe_started.notified().await;
    worker.mark_idle();
    release_probe.notify_one();

    assert!(
        tokio::time::timeout(Duration::from_millis(250), wait)
            .await
            .expect("idle notification must avoid the full grace")
            .expect("wait task"),
        "a fresh idle probe must observe the completed replay"
    );
    registry.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_during_pending_probe_keeps_worker_non_idle() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let cancellation = Arc::new(ProfileHostAdmissionCancellation::new());
    let probe_started = Arc::new(Notify::new());
    let release_probe = Arc::new(Notify::new());
    let pending_count_override: PendingReplayCountOverride = {
        let probe_started = Arc::clone(&probe_started);
        let release_probe = Arc::clone(&release_probe);
        Arc::new(move || {
            let probe_started = Arc::clone(&probe_started);
            let release_probe = Arc::clone(&release_probe);
            Box::pin(async move {
                probe_started.notify_one();
                release_probe.notified().await;
                0
            })
        })
    };
    let worker = Arc::new(ProfileHostAdmissionReplayWorker::new(
        &broker,
        &profile_root,
        cancellation,
        None,
        Some(pending_count_override),
    ));

    let probe_worker = Arc::clone(&worker);
    let probe = tokio::spawn(async move { probe_worker.is_idle().await });
    probe_started.notified().await;
    worker.kick();
    release_probe.notify_one();

    assert!(
        !probe.await.expect("idle probe task"),
        "a concurrent kick must keep replay readiness non-idle"
    );
}

#[tokio::test]
async fn shutdown_never_reports_replay_ready() {
    let registry = ProfileHostAdmissionReplayRegistry::default();
    registry.shutdown().await;

    assert!(
        !registry
            .wait_idle(
                Path::new("missing-user-sessions.db"),
                Duration::from_secs(1)
            )
            .await,
        "shutdown replay authority must never report ready"
    );
}

#[tokio::test]
async fn shutdown_until_preserves_bootstrap_task_panic() {
    let registry = ProfileHostAdmissionReplayRegistry::default();
    let worker = Arc::new(ProfileHostAdmissionBootstrapWorker::new(
        Arc::clone(&registry.cancellation),
        BOOTSTRAP_RETRY_BUDGET,
    ));
    registry.bootstrap_workers.lock().await.insert(
        PathBuf::from("panicked-bootstrap"),
        ProfileHostAdmissionBootstrapEntry {
            worker,
            task: tokio::spawn(async {
                panic!("host admission bootstrap shutdown task panic");
            }),
        },
    );

    let status = registry
        .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(10))
        .await;

    assert!(matches!(
        status,
        ShutdownStatus::Failed(error)
            if error.contains("host admission bootstrap shutdown task panic")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_worker_is_evicted_after_the_bound() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let db_path = crate::sessions::user_sessions_db_path(&profile_root);
    let (runtime, _) =
        crate::application::host_admission::HostAdmissionRuntime::open_for_database(&db_path)
            .unwrap();
    let broker = Arc::new(crate::application::host_admission::HostAdmissionBroker::new(runtime));
    let registry =
        ProfileHostAdmissionReplayRegistry::with_idle_eviction_after(Duration::from_millis(20));
    let pass_override = Arc::new(|| {
        Box::pin(async { HostAdmissionOutcome::accepted_for_replay() })
            as std::pin::Pin<Box<dyn std::future::Future<Output = HostAdmissionOutcome> + Send>>
    });

    registry
        .ensure_with_pass_override(&db_path, &profile_root, &broker, pass_override)
        .await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.worker_count().await != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idle replay worker must be evicted");
}
