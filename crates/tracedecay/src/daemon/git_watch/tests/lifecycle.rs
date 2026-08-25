use super::*;

use crate::daemon::git_watch::ownership::retire_missing_repository_owners;

#[tokio::test]
async fn pinned_project_config_is_the_only_activation_authority() {
    let repo = temp_repo();
    let process_default = SyncConfig::default();
    assert!(!process_default.auto_watch);
    let watcher = GitWatcher::from_parts(
        process_default.clone(),
        true,
        MaintenanceCoordinator::default(),
        Some(crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(32)),
    );

    assert_eq!(
        watcher
            .ensure_watching_with_config(repo.path(), &process_default)
            .await,
        GitWatcherAdmission::Disabled
    );
    assert!(watcher.inner.projects.lock().await.is_empty());

    let mut pinned = process_default;
    pinned.auto_watch = true;
    assert_eq!(
        watcher
            .ensure_watching_with_config(repo.path(), &pinned)
            .await,
        GitWatcherAdmission::Ready,
        "an exact pinned project opt-in must activate even when process defaults are off"
    );
    assert_eq!(watcher.inner.projects.lock().await.len(), 1);
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn linked_worktree_registration_reconciles_its_pinned_timing() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let mut primary_config = fast_watch_config();
    primary_config.watch_debounce_ms = 90;
    primary_config.watch_max_delay_ms = 900;
    primary_config.backstop_interval_mins = 9;
    let watcher = GitWatcher::new(primary_config.clone());

    assert_eq!(
        watcher
            .ensure_watching_with_config(&primary, &primary_config)
            .await,
        GitWatcherAdmission::Ready
    );
    let mut linked_config = primary_config;
    linked_config.watch_debounce_ms = 15;
    linked_config.watch_max_delay_ms = 150;
    linked_config.backstop_interval_mins = 3;
    assert_eq!(
        watcher
            .ensure_watching_with_config(&linked, &linked_config)
            .await,
        GitWatcherAdmission::Ready
    );

    let state = ready_registered_state(&watcher, &primary).await;
    let timing = state.effective_timing();
    assert_eq!(timing.debounce, Duration::from_millis(15));
    assert_eq!(timing.max_delay, Duration::from_millis(150));
    assert_eq!(timing.backstop_interval, Some(Duration::from_mins(3)));
    assert_eq!(
        state.config_for_root(&linked.canonicalize().expect("linked canonical root")),
        Some(linked_config),
        "the shared repository owner must retain the exact linked-worktree pin"
    );
    assert!(watcher.shutdown().await.is_clean());
}

#[test]
fn metadata_paths_route_to_exact_worktree_or_shared_reconciliation() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let common = crate::worktree::git_common_dir(&primary).expect("git common directory");
    let primary_root = primary.canonicalize().expect("primary root");
    let linked_root = linked.canonicalize().expect("linked root");
    let linked_git_dir = worktree_git_dir(&linked).expect("linked git directory");
    let state = Arc::new(WatchState::new(
        common.clone(),
        primary_root,
        worktree_git_dir(&primary).expect("primary git directory"),
        MaintenanceCoordinator::default(),
    ));
    assert!(matches!(
        state.register_worktree(linked_root.clone(), linked_git_dir.clone(), 8),
        WorktreeRegistration::Ready
    ));

    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![linked_git_dir.join("HEAD")],
            attrs: notify::event::EventAttributes::default(),
        },
    );
    {
        let mut dirty = state.dirty.blocking_lock();
        assert_eq!(dirty.affected_roots, BTreeSet::from([linked_root]));
        assert!(!dirty.reconcile_metadata);
        assert!(dirty.take());
    }

    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![common.join("refs/heads/main")],
            attrs: notify::event::EventAttributes::default(),
        },
    );
    assert!(
        state.dirty.blocking_lock().reconcile_metadata,
        "shared refs truthfully widen to every mounted sibling"
    );
}

#[test]
fn linked_operation_marker_does_not_starve_exact_sibling_frontier() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let primary_root = primary.canonicalize().expect("primary root");
    let linked_root = linked.canonicalize().expect("linked root");
    let linked_git_dir = worktree_git_dir(&linked).expect("linked git directory");
    let state = WatchState::new(
        crate::worktree::git_common_dir(&primary).expect("git common directory"),
        primary_root.clone(),
        worktree_git_dir(&primary).expect("primary git directory"),
        MaintenanceCoordinator::default(),
    );
    assert!(matches!(
        state.register_worktree(linked_root.clone(), linked_git_dir.clone(), 8),
        WorktreeRegistration::Ready
    ));
    std::fs::create_dir(linked_git_dir.join("rebase-merge")).expect("operation marker");
    let daemon_cancellation = tracedecay_usecases::context::CancellationToken::new();
    let cancellation = state.cancellation(&daemon_cancellation);

    assert!(matches!(
        operation_state_blocking(
            &state,
            8,
            &cancellation,
            StdInstant::now() + GIT_OBSERVATION_BUDGET,
            Some(&BTreeSet::from([primary_root])),
        ),
        OperationObservation::State(OperationState::Idle)
    ));
    assert!(matches!(
        operation_state_blocking(
            &state,
            8,
            &cancellation,
            StdInstant::now() + GIT_OBSERVATION_BUDGET,
            Some(&BTreeSet::from([linked_root])),
        ),
        OperationObservation::State(OperationState::InFlight)
    ));
}

#[test]
fn deferred_freshness_retry_is_single_and_backoff_bounded() {
    let state = WatchState::new(
        PathBuf::from("/repo/.git"),
        PathBuf::from("/repo"),
        PathBuf::from("/repo/.git"),
        MaintenanceCoordinator::default(),
    );
    let started = Instant::now();
    state.schedule_retry();
    let first = state.retry_not_before().expect("first retry");
    state.schedule_retry();
    let merged = state.retry_not_before().expect("merged retry");
    assert!(merged > first);

    for _ in 0..32 {
        state.schedule_retry();
    }
    assert!(
        state.retry_not_before().expect("bounded retry") <= Instant::now() + Duration::from_mins(1),
        "retry amplification must cap at one minute"
    );
    assert!(first > started);
    state.clear_retry();
    assert!(state.retry_not_before().is_none());
}

#[tokio::test]
async fn concurrent_spawn_retains_one_backstop_task() {
    let mut config = fast_watch_config();
    config.backstop_interval_mins = 1;
    let watcher = GitWatcher::new(config);

    let retained = watcher.inner.backstop_task.lock().await;
    let mut left = Box::pin(watcher.spawn());
    let mut right = Box::pin(watcher.spawn());
    assert!(futures_util::poll!(&mut left).is_pending());
    assert!(futures_util::poll!(&mut right).is_pending());
    drop(retained);
    let (left, right) = tokio::join!(left, right);
    assert!(
        matches!(
            (left, right),
            (GitWatcherStart::Started, GitWatcherStart::AlreadyStarted)
                | (GitWatcherStart::AlreadyStarted, GitWatcherStart::Started)
        ),
        "exactly one concurrent caller must start the backstop: {left:?}, {right:?}"
    );
    let first_task = watcher
        .inner
        .backstop_task
        .lock()
        .await
        .as_ref()
        .expect("first start must retain its backstop task")
        .id();
    let (repeated_left, repeated_right) = tokio::join!(watcher.spawn(), watcher.spawn());
    assert_eq!(repeated_left, GitWatcherStart::AlreadyStarted);
    assert_eq!(repeated_right, GitWatcherStart::AlreadyStarted);
    let repeated_task = watcher
        .inner
        .backstop_task
        .lock()
        .await
        .as_ref()
        .expect("repeated start must retain the backstop task")
        .id();

    assert_eq!(
        repeated_task, first_task,
        "repeated start must not overwrite and detach the retained backstop"
    );
    watcher.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_publication_linearizes_before_racing_shutdown() {
    let watcher = GitWatcher::new(fast_watch_config());
    watcher.inner.spawn_publication_probe.arm();
    let start = {
        let watcher = watcher.clone();
        tokio::spawn(async move { watcher.spawn().await })
    };
    tokio::time::timeout(
        TEST_READY_TIMEOUT,
        watcher.inner.spawn_publication_probe.entered.notified(),
    )
    .await
    .expect("start reaches the checked publication boundary");

    let shutdown_requested = watcher.inner.shutdown_requested.notified();
    let cancellation = {
        let watcher = watcher.clone();
        tokio::task::spawn_blocking(move || watcher.cancel())
    };
    tokio::time::timeout(TEST_READY_TIMEOUT, shutdown_requested)
        .await
        .expect("shutdown reaches the lifecycle fence");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    watcher.inner.spawn_publication_probe.release();

    assert_eq!(start.await.expect("start task"), GitWatcherStart::Started);
    cancellation.await.expect("cancellation task");
    let spawn_order = watcher.inner.lifecycle_receipts.spawn();
    let shutdown_order = watcher.inner.lifecycle_receipts.shutdown();
    assert!(
        spawn_order > 0 && shutdown_order > spawn_order,
        "a checked start must publish before shutdown: spawn={spawn_order}, shutdown={shutdown_order}"
    );
    assert_eq!(watcher.spawn().await, GitWatcherStart::ShuttingDown);
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn concurrent_repository_admission_retains_one_supervisor_task() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());

    assert_eq!(
        watcher.ensure_watching(repo.path()).await,
        GitWatcherAdmission::Ready
    );
    let state = ready_registered_state(&watcher, repo.path()).await;
    let first_task = state
        .retained_task_id()
        .expect("repository admission must retain its supervisor");

    let (left, right) = tokio::join!(
        watcher.ensure_watching(repo.path()),
        watcher.ensure_watching(repo.path())
    );
    assert_eq!(left, GitWatcherAdmission::Ready);
    assert_eq!(right, GitWatcherAdmission::Ready);
    let repeated_task = state
        .retained_task_id()
        .expect("repeated admission must retain the supervisor");

    assert_eq!(
        repeated_task, first_task,
        "repeated admission must not overwrite and detach the retained supervisor"
    );
    assert_eq!(
        watcher.inner.projects.lock().await.len(),
        1,
        "one common repository must retain one watcher authority"
    );
    watcher.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repository_publication_linearizes_before_racing_shutdown() {
    let repository = temp_repo();
    let root = repository.path().to_path_buf();
    let watcher = GitWatcher::new(fast_watch_config());
    watcher.inner.repository_publication_probe.arm();
    let admission = {
        let watcher = watcher.clone();
        tokio::spawn(async move { watcher.ensure_watching(&root).await })
    };
    tokio::time::timeout(
        TEST_READY_TIMEOUT,
        watcher
            .inner
            .repository_publication_probe
            .entered
            .notified(),
    )
    .await
    .expect("admission reaches the checked publication boundary");

    let shutdown_requested = watcher.inner.shutdown_requested.notified();
    let cancellation = {
        let watcher = watcher.clone();
        tokio::task::spawn_blocking(move || watcher.cancel())
    };
    tokio::time::timeout(TEST_READY_TIMEOUT, shutdown_requested)
        .await
        .expect("shutdown reaches the lifecycle fence");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    watcher.inner.repository_publication_probe.release();

    assert_eq!(
        admission.await.expect("admission task"),
        GitWatcherAdmission::Ready
    );
    cancellation.await.expect("cancellation task");
    let repository_order = watcher.inner.lifecycle_receipts.repository();
    let shutdown_order = watcher.inner.lifecycle_receipts.shutdown();
    assert!(
        repository_order > 0 && shutdown_order > repository_order,
        "a checked admission must publish before shutdown: repository={repository_order}, shutdown={shutdown_order}"
    );
    assert_eq!(
        watcher.ensure_watching(repository.path()).await,
        GitWatcherAdmission::ShuttingDown
    );
    assert_eq!(watcher.inner.projects.lock().await.len(), 1);
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_registration_linearizes_before_racing_shutdown() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let watcher = GitWatcher::new(fast_watch_config());
    assert_eq!(
        watcher.ensure_watching(&primary).await,
        GitWatcherAdmission::Ready
    );
    watcher.inner.repository_publication_probe.arm();
    let registration = {
        let watcher = watcher.clone();
        let linked = linked.clone();
        tokio::spawn(async move { watcher.ensure_watching(&linked).await })
    };
    tokio::time::timeout(
        TEST_READY_TIMEOUT,
        watcher
            .inner
            .repository_publication_probe
            .entered
            .notified(),
    )
    .await
    .expect("registration reaches the checked publication boundary");

    let shutdown_requested = watcher.inner.shutdown_requested.notified();
    let cancellation = {
        let watcher = watcher.clone();
        tokio::task::spawn_blocking(move || watcher.cancel())
    };
    tokio::time::timeout(TEST_READY_TIMEOUT, shutdown_requested)
        .await
        .expect("shutdown reaches the lifecycle fence");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    watcher.inner.repository_publication_probe.release();

    assert_eq!(
        registration.await.expect("registration task"),
        GitWatcherAdmission::Ready
    );
    cancellation.await.expect("cancellation task");
    let registration_order = watcher.inner.lifecycle_receipts.registration();
    let shutdown_order = watcher.inner.lifecycle_receipts.shutdown();
    assert!(
        registration_order > 0 && shutdown_order > registration_order,
        "a checked linked registration must publish before shutdown: registration={registration_order}, shutdown={shutdown_order}"
    );
    let state = ready_registered_state(&watcher, &primary).await;
    assert!(state.contains_worktree(&linked.canonicalize().unwrap()));
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn concurrent_shutdown_waits_for_retained_join_completion() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let state = Arc::new(WatchState::new(
        crate::worktree::git_common_dir(repo.path()).expect("git common directory"),
        repo.path().canonicalize().expect("canonical project root"),
        worktree_git_dir(repo.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    ));
    let task_release = Arc::new(Notify::new());
    let owned_task = {
        let task_release = Arc::clone(&task_release);
        tokio::spawn(async move {
            task_release.notified().await;
        })
    };
    state.retain_task(owned_task);
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(state.common_dir.clone(), Arc::clone(&state));

    let mut first = Box::pin(watcher.shutdown());
    assert!(
        futures_util::poll!(&mut first).is_pending(),
        "the first shutdown caller must wait for the retained join"
    );
    let mut repeated = Box::pin(watcher.shutdown());
    assert!(
        futures_util::poll!(&mut repeated).is_pending(),
        "a concurrent shutdown caller must wait for the retained join"
    );

    task_release.notify_one();
    first.await;
    repeated.await;
    assert!(!state.has_retained_task());
}

#[tokio::test(start_paused = true)]
async fn shutdown_bounds_a_stuck_repository_join() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let state = Arc::new(WatchState::new(
        crate::worktree::git_common_dir(repo.path()).expect("git common directory"),
        repo.path().canonicalize().expect("canonical project root"),
        worktree_git_dir(repo.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    ));
    state.retain_task(tokio::spawn(std::future::pending::<()>()));
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(state.common_dir.clone(), Arc::clone(&state));

    let outcome = tokio::time::timeout(
        GIT_OBSERVATION_BUDGET + Duration::from_millis(1),
        watcher.shutdown(),
    )
    .await
    .expect("shutdown must not wait beyond the watcher observation budget");

    assert_eq!(
        outcome.failures(),
        &[GitWatcherTaskFailure {
            owner: GitWatcherTaskOwner::Repository(state.common_dir.clone()),
            kind: GitWatcherTaskFailureKind::TimedOut,
        }]
    );
    assert!(!state.has_retained_task());
}

#[tokio::test]
async fn an_unmounted_scheduler_does_not_retry_the_exact_watcher_frontier() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    assert_eq!(
        watcher.ensure_watching(repo.path()).await,
        GitWatcherAdmission::Ready
    );
    let state = ready_registered_state(&watcher, repo.path()).await;

    request_freshness_for_repository(&watcher.inner, &state, None).await;

    assert!(
        watcher
            .inner
            .projects
            .lock()
            .await
            .contains_key(&state.common_dir),
        "an existing checkout remains observable for a later scheduler mount"
    );
    assert!(
        state.retry_not_before().is_none(),
        "an unmounted scheduler is terminal for this frontier, not an infinite retry"
    );
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn missing_owner_is_joined_and_capacity_can_remount() {
    let container = tempfile::tempdir().expect("repository container");
    let first_root = container.path().join("first");
    let second_root = container.path().join("second");
    seed_repo(&first_root);
    seed_repo(&second_root);
    let mut config = fast_watch_config();
    config.watch_max_projects = 1;
    let watcher = GitWatcher::new(config);

    assert_eq!(
        watcher.ensure_watching(&first_root).await,
        GitWatcherAdmission::Ready
    );
    let retired = ready_registered_state(&watcher, &first_root).await;
    std::fs::remove_dir_all(&first_root).expect("remove first repository");

    assert_eq!(
        watcher.ensure_watching(&second_root).await,
        GitWatcherAdmission::Ready,
        "a missing owner must release repository capacity"
    );
    assert!(
        !retired.has_retained_task(),
        "eviction must join the retired repository supervisor"
    );

    std::fs::remove_dir_all(&second_root).expect("remove second repository");
    seed_repo(&first_root);
    assert_eq!(
        watcher.ensure_watching(&first_root).await,
        GitWatcherAdmission::Ready,
        "a recreated repository must mount after its stale owner retires"
    );
    let remounted = ready_registered_state(&watcher, &first_root).await;
    assert!(
        !Arc::ptr_eq(&retired, &remounted),
        "a recreated repository must receive a fresh watcher owner"
    );
    assert!(!retired.has_retained_task());
    assert!(watcher.shutdown().await.is_clean());
}

#[test]
fn pruning_the_last_root_retires_registration_authority() {
    let repository = temp_repo();
    let root = repository.path().to_path_buf();
    let state = WatchState::new(
        crate::worktree::git_common_dir(&root).expect("git common directory"),
        root,
        worktree_git_dir(repository.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    );
    let daemon_cancellation = tracedecay_usecases::context::CancellationToken::new();
    let cancellation = state.cancellation(&daemon_cancellation);
    drop(repository);

    assert!(state.prune_missing_worktrees(|| false));
    assert!(
        !cancellation.is_cancelled(),
        "pruning alone must leave the owner live so a concurrent registration can win"
    );
    assert!(state.retire_if_empty());
    assert!(
        cancellation.is_cancelled(),
        "the zero-root transition must retire under the registration authority"
    );
}

#[tokio::test]
async fn retired_linked_owner_is_replaced_before_recreated_root_admission() {
    let (_container, primary, linked) = linked_worktree_fixture();
    let unrelated = temp_repo();
    let mut config = fast_watch_config();
    config.watch_max_projects = 1;
    let watcher = GitWatcher::new(config);
    let common = crate::worktree::git_common_dir(&primary).expect("git common directory");
    let stale = Arc::new(WatchState::new(
        common.clone(),
        linked.canonicalize().expect("canonical linked root"),
        worktree_git_dir(&linked).expect("linked git directory"),
        MaintenanceCoordinator::default(),
    ));
    stale.retire();
    let stale_task_release = Arc::new(Notify::new());
    stale.retain_task({
        let stale_task_release = Arc::clone(&stale_task_release);
        tokio::spawn(async move { stale_task_release.notified().await })
    });
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(common, Arc::clone(&stale));

    let first_admission = {
        let watcher = watcher.clone();
        let linked = linked.clone();
        tokio::spawn(async move { watcher.ensure_watching(&linked).await })
    };
    tokio::time::timeout(TEST_READY_TIMEOUT, async {
        while stale.has_retained_task() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement starts by joining the retired supervisor");
    let mut racing_admission = Box::pin(watcher.ensure_watching(&linked));
    assert!(
        futures_util::poll!(&mut racing_admission).is_pending(),
        "a racing recreation must wait until the retired supervisor is joined"
    );
    stale_task_release.notify_one();
    // Both admissions must be polled together: the racing admission already
    // queued as the fair projects-mutex head waiter, so awaiting the first
    // admission alone would strand the lock handoff and self-deadlock the
    // test while the production fence behaves correctly.
    let (first_admission, racing_admission) = tokio::time::timeout(TEST_READY_TIMEOUT, async {
        tokio::join!(first_admission, racing_admission)
    })
    .await
    .expect("both admissions complete once the retired supervisor is joined");
    assert_eq!(
        first_admission.expect("first admission task"),
        GitWatcherAdmission::Ready
    );
    assert_eq!(racing_admission, GitWatcherAdmission::Ready);
    let active = ready_registered_state(&watcher, &linked).await;
    assert!(
        !Arc::ptr_eq(&active, &stale),
        "a retired owner must be evicted even when it still carries a linked root"
    );
    assert!(active.contains_worktree(&linked.canonicalize().unwrap()));
    assert!(active.has_retained_task());
    assert_eq!(
        watcher.ensure_watching(unrelated.path()).await,
        GitWatcherAdmission::Capacity,
        "the replacement must reuse exactly one repository slot"
    );
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn concurrent_registration_prevents_empty_owner_retirement() {
    let container = tempfile::tempdir().expect("repository container");
    let missing_root = container.path().join("missing");
    let replacement = container.path().join("replacement");
    seed_repo(&replacement);
    let watcher = GitWatcher::new(fast_watch_config());
    let common_dir =
        crate::worktree::git_common_dir(&replacement).expect("replacement common directory");
    let state = Arc::new(WatchState::new(
        common_dir.clone(),
        missing_root,
        common_dir.join("missing-git-dir"),
        MaintenanceCoordinator::default(),
    ));
    state.retain_task(tokio::spawn(async {}));
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(common_dir.clone(), Arc::clone(&state));
    state.retirement_probe.arm();

    let inner = Arc::clone(&watcher.inner);
    let retirement = tokio::spawn(async move {
        retire_missing_repository_owners(&inner).await;
    });
    state.retirement_probe.after_empty.notified().await;
    assert!(matches!(
        state.register_worktree(
            replacement
                .canonicalize()
                .expect("replacement canonical root"),
            worktree_git_dir(&replacement).expect("replacement git directory"),
            8,
        ),
        WorktreeRegistration::Ready
    ));
    state.retirement_probe.release.notify_one();
    retirement.await.expect("retirement task");

    assert!(
        watcher
            .inner
            .projects
            .lock()
            .await
            .contains_key(&common_dir),
        "registration that wins before final retirement must keep the owner live"
    );
    assert!(watcher.shutdown().await.is_clean());
}

#[tokio::test]
async fn explicit_metadata_watch_plan_fails_closed_at_its_directory_cap() {
    let repo = temp_repo();
    let common_dir = crate::worktree::git_common_dir(repo.path()).expect("git common directory");
    for index in 0..=MAX_METADATA_WATCH_DIRECTORIES {
        std::fs::create_dir_all(common_dir.join(format!("refs/heads/team-{index}/nested")))
            .expect("create nested ref directory");
    }
    let state = Arc::new(WatchState::new(
        common_dir,
        repo.path().canonicalize().expect("canonical project root"),
        worktree_git_dir(repo.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    ));
    let cancellation = state.cancellation(&tracedecay_usecases::context::CancellationToken::new());

    assert_eq!(
        observe_watch_plan(state, cancellation).await,
        Err(WatchPlanFailure::Capacity),
        "nested ref namespaces must degrade instead of recursively amplifying OS watches"
    );
}

#[tokio::test]
async fn a_new_nested_ref_directory_requests_a_watch_plan_rebuild() {
    let repo = temp_repo();
    let common_dir = crate::worktree::git_common_dir(repo.path()).expect("git common directory");
    let state = Arc::new(WatchState::new(
        common_dir.clone(),
        repo.path().canonicalize().expect("canonical project root"),
        worktree_git_dir(repo.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    ));
    let nested = common_dir.join("refs/heads/team/new");
    std::fs::create_dir_all(&nested).expect("create nested ref namespace");

    classify_and_mark(
        &state,
        &notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::Folder),
            paths: vec![nested.clone()],
            attrs: notify::event::EventAttributes::default(),
        },
    );

    tokio::time::timeout(Duration::from_millis(50), state.reconfigure.notified())
        .await
        .expect("a nested metadata directory must rebuild the explicit watch plan");
    let cancellation = state.cancellation(&tracedecay_usecases::context::CancellationToken::new());
    let plan = observe_watch_plan(state, cancellation)
        .await
        .expect("rebuilt watch plan");
    assert!(
        plan.contains(&nested),
        "the rebuilt plan must include the newly created nested ref directory"
    );
}

#[test]
fn notify_capacity_is_typed_health_and_reconciliation_evidence() {
    let state = WatchState::new(
        PathBuf::from("/repo/.git"),
        PathBuf::from("/repo"),
        PathBuf::from("/repo/.git"),
        MaintenanceCoordinator::default(),
    );
    let error = notify::Error::new(notify::ErrorKind::MaxFilesWatch);

    mark_notify_failure(&state, &error);

    assert_eq!(
        state.health.snapshot().status,
        ProjectWatchStatus::NotifyCapacity
    );
    assert!(state.reconciliation_pending.load(Ordering::Acquire));
}

#[tokio::test]
async fn shutdown_reports_cancelled_repository_join() {
    let repo = temp_repo();
    let watcher = GitWatcher::new(fast_watch_config());
    let state = Arc::new(WatchState::new(
        crate::worktree::git_common_dir(repo.path()).expect("git common directory"),
        repo.path().canonicalize().expect("canonical project root"),
        worktree_git_dir(repo.path()).expect("worktree git directory"),
        MaintenanceCoordinator::default(),
    ));
    let task = tokio::spawn(std::future::pending::<()>());
    task.abort();
    state.retain_task(task);
    watcher
        .inner
        .projects
        .lock()
        .await
        .insert(state.common_dir.clone(), Arc::clone(&state));

    let outcome = watcher.shutdown().await;

    assert_eq!(
        outcome.failures(),
        &[GitWatcherTaskFailure {
            owner: GitWatcherTaskOwner::Repository(state.common_dir.clone()),
            kind: GitWatcherTaskFailureKind::Cancelled,
        }]
    );
}
