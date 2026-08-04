use super::*;

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
        .task
        .lock()
        .await
        .as_ref()
        .expect("repository admission must retain its supervisor")
        .id();

    let (left, right) = tokio::join!(
        watcher.ensure_watching(repo.path()),
        watcher.ensure_watching(repo.path())
    );
    assert_eq!(left, GitWatcherAdmission::Ready);
    assert_eq!(right, GitWatcherAdmission::Ready);
    let repeated_task = state
        .task
        .lock()
        .await
        .as_ref()
        .expect("repeated admission must retain the supervisor")
        .id();

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
    *state.task.lock().await = Some(owned_task);
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
    assert!(state.task.lock().await.is_none());
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
        retired.task.lock().await.is_none(),
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
    assert!(retired.task.lock().await.is_none());
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
    let cancellation = state.cancellation(&crate::application::context::CancellationToken::new());

    assert_eq!(
        observe_watch_plan(state, cancellation).await,
        Err(WatchPlanFailure::Capacity),
        "nested ref namespaces must degrade instead of recursively amplifying OS watches"
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
    *state.task.lock().await = Some(task);
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
