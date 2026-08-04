use std::sync::Arc;

use crate::daemon::{
    DaemonHandshake, DaemonLifecycle, ProjectOpenTaskClaim, ProjectOpenTasks, StoreAdministration,
};
use crate::errors::{Result, TraceDecayError};
#[cfg(unix)]
use tempfile::TempDir;

use super::project_open_test_route;

#[tokio::test]
async fn project_open_task_shutdown_cancels_and_clears_route_registry() {
    let tasks = ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown");
    let started = Arc::new(tokio::sync::Notify::new());
    let task_started = Arc::clone(&started);
    let state = match tasks
        .start_cancellable(route, move |cancellation| async move {
            task_started.notify_one();
            cancellation.cancelled().await;
            Err(TraceDecayError::Config {
                message: "project open cancelled".to_string(),
            })
        })
        .await
    {
        ProjectOpenTaskClaim::InFlight(state) => state,
        ProjectOpenTaskClaim::Failed(_) => panic!("pending task must start"),
        ProjectOpenTaskClaim::Saturated => panic!("pending task must fit"),
    };
    started.notified().await;
    assert_eq!(tasks.tracked_task_count().await, 1);

    tasks.shutdown().await;

    assert_eq!(tasks.tracked_task_count().await, 0);
    assert_eq!(tasks.tracked_route_count().await, 0);
    assert!(
        ProjectOpenTasks::wait_for_completion(state).await.is_err(),
        "cancelled open tasks must wake waiters instead of retaining them"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_open_shutdown_waits_for_safe_unit_then_joins() {
    let tasks = ProjectOpenTasks::default();
    let route = project_open_test_route("cooperative-shutdown");
    let lifecycle = DaemonLifecycle::default();
    let store_administration = StoreAdministration::default();
    let (cancellation_tx, cancellation_rx) = tokio::sync::oneshot::channel();
    let (unit_started_tx, unit_started_rx) = tokio::sync::oneshot::channel();
    let (unit_release_tx, unit_release_rx) = tokio::sync::oneshot::channel();
    let (unit_finished_tx, unit_finished_rx) = tokio::sync::oneshot::channel();

    let task_lifecycle = lifecycle.clone();
    let task_administration = store_administration.clone();
    let state = match tasks
        .start_cancellable(route, move |cancellation| async move {
            let _activity = task_lifecycle
                .try_enter()
                .expect("project open lifecycle activity");
            let published_cancellation = cancellation.clone();
            task_administration
                .with_writer_until_cancelled(&cancellation, move || async move {
                    cancellation_tx
                        .send(published_cancellation)
                        .expect("publish project-open cancellation");
                    unit_started_tx.send(()).expect("publish safe unit start");
                    unit_release_rx.await.expect("release safe unit");
                    unit_finished_tx
                        .send(())
                        .expect("publish safe unit completion");
                })
                .await
                .expect("safe unit acquired writer administration");
            cancellation.cancelled().await;
            Err(TraceDecayError::Config {
                message: "project open cancelled after safe unit".to_string(),
            })
        })
        .await
    {
        ProjectOpenTaskClaim::InFlight(state) => state,
        ProjectOpenTaskClaim::Failed(_) => panic!("project open must start"),
        ProjectOpenTaskClaim::Saturated => panic!("project open must fit"),
    };
    let cancellation = cancellation_rx
        .await
        .expect("project-open cancellation token");
    unit_started_rx.await.expect("safe unit started");

    let shutdown_tasks = tasks.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_tasks.shutdown().await });
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        cancellation.cancelled(),
    )
    .await
    .expect("shutdown must request cooperative cancellation");
    assert!(
        !shutdown.is_finished(),
        "shutdown must not abort a transactionally safe unit in progress"
    );

    unit_release_tx.send(()).expect("release safe unit");
    unit_finished_rx.await.expect("safe unit completed");
    let cooperative = tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut shutdown)
        .await
        .expect("cooperative project-open shutdown timed out")
        .expect("project-open shutdown task");
    assert!(
        cooperative.is_clean(),
        "normal warm-up cancellation must not reach its timeout guard: {cooperative:?}"
    );
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        lifecycle.wait_for_idle(),
    )
    .await
    .expect("client-drain lifecycle activity must be released");
    tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        store_administration.with_writer(|| async {}),
    )
    .await
    .expect("server shutdown must reacquire writer administration");
    assert_eq!(tasks.tracked_route_count().await, 0);
    ProjectOpenTasks::wait_for_completion(state)
        .await
        .expect_err("cancelled project open must report a terminal failure");
}

#[tokio::test]
async fn project_open_shutdown_backstop_aborts_and_joins_noncooperative_task() {
    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    let tasks = ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown-backstop");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    match tasks
        .start_cancellable(route, move |_| async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            started_tx.send(()).expect("publish task start");
            std::future::pending::<Result<()>>().await
        })
        .await
    {
        ProjectOpenTaskClaim::InFlight(_) => {}
        ProjectOpenTaskClaim::Failed(_) => panic!("pending task must start"),
        ProjectOpenTaskClaim::Saturated => panic!("pending task must fit"),
    }
    started_rx.await.expect("noncooperative task started");

    let cooperative = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        tasks.shutdown_with_deadline(
            tokio::time::Duration::ZERO,
            tokio::time::Duration::from_secs(1),
        ),
    )
    .await
    .expect("shutdown backstop must join the aborted task");

    assert!(
        !cooperative.is_clean(),
        "noncooperative task must reach the backstop"
    );
    dropped_rx
        .await
        .expect("joined task must drop its owned resources before shutdown returns");
    assert_eq!(tasks.tracked_route_count().await, 0);
}

#[tokio::test(start_paused = true)]
async fn project_open_shutdown_until_reserves_time_to_join_aborted_tasks() {
    struct Dropped(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    let tasks = ProjectOpenTasks::default();
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_dropped = Arc::clone(&dropped);
    match tasks
        .start_cancellable(
            project_open_test_route("shutdown-until-backstop"),
            move |_| async move {
                let _dropped = Dropped(task_dropped);
                std::future::pending::<Result<()>>().await
            },
        )
        .await
    {
        ProjectOpenTaskClaim::InFlight(_) => {}
        ProjectOpenTaskClaim::Failed(_) => panic!("pending task must start"),
        ProjectOpenTaskClaim::Saturated => panic!("pending task must fit"),
    }
    tokio::task::yield_now().await;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

    assert!(
        !tasks.shutdown_until(deadline).await.is_clean(),
        "noncooperative task must be reported as timed out"
    );
    assert!(
        dropped.load(std::sync::atomic::Ordering::Acquire),
        "shutdown must join the aborted task before returning"
    );
    assert!(
        tokio::time::Instant::now() < deadline,
        "abort join must use the reserved portion of the global deadline"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_open_shutdown_detaches_synchronous_work_after_abort_deadline() {
    let tasks = ProjectOpenTasks::default();
    let route = project_open_test_route("shutdown-synchronous-backstop");
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_started = Arc::clone(&started);
    let task_release = Arc::clone(&release);
    match tasks
        .start_cancellable(route, move |_| async move {
            task_started.store(true, std::sync::atomic::Ordering::Release);
            while !task_release.load(std::sync::atomic::Ordering::Acquire) {
                std::hint::spin_loop();
            }
            Ok(())
        })
        .await
    {
        ProjectOpenTaskClaim::InFlight(_) => {}
        ProjectOpenTaskClaim::Failed(_) => panic!("synchronous task must start"),
        ProjectOpenTaskClaim::Saturated => panic!("synchronous task must fit"),
    }
    while !started.load(std::sync::atomic::Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    let cooperative = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        tasks.shutdown_with_deadline(
            tokio::time::Duration::ZERO,
            tokio::time::Duration::from_millis(25),
        ),
    )
    .await
    .expect("shutdown must detach synchronous work after its abort deadline");

    assert!(
        !cooperative.is_clean(),
        "synchronous work must reach the backstop"
    );
    assert_eq!(tasks.tracked_route_count().await, 0);
    release.store(true, std::sync::atomic::Ordering::Release);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn project_server_shutdown_retries_after_contended_registry_detach() {
    let temp = TempDir::new().expect("project server fixture");
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    std::fs::create_dir_all(project.join("src")).expect("fixture source directory");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("fixture source");
    let client_identity = super::super::test_client_identity_for(profile_root.clone());
    super::super::initialize_test_project(&project, &client_identity).await;
    let _database_scope = super::super::enter_test_daemon_database_scope(
        &profile_root,
        "project server shutdown retry after registry contention",
    );
    let engine = super::super::test_daemon_engine_for_profile(&profile_root);
    let handshake = DaemonHandshake {
        project_path: Some(project),
        client_identity,
        ..super::super::test_handshake_defaults()
    };
    let server = engine
        .project_server(&handshake)
        .await
        .expect("open project server");
    let store_administration = engine.store_administration.clone();
    let registry = store_administration.project_servers().lock().await;

    let first =
        crate::daemon::shutdown_project_servers(tokio::time::Instant::now(), &store_administration)
            .await;

    assert!(
        first.outcomes.iter().any(|outcome| {
            outcome.owner == "project_server_detach"
                && outcome.status == crate::daemon::ShutdownStatus::TimedOut
        }),
        "contended detach must emit a typed timeout receipt: {first:?}"
    );
    assert!(
        registry
            .values()
            .any(|registered| Arc::ptr_eq(registered, &server)),
        "a contended registry must retain the project server for the retry"
    );
    drop(registry);

    let retry = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        crate::daemon::shutdown_project_servers(
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(10),
            &store_administration,
        ),
    )
    .await
    .expect("shutdown retry must finish");

    assert!(retry.is_clean(), "shutdown retry receipt: {retry:?}");
    assert!(
        store_administration
            .project_servers()
            .lock()
            .await
            .servers
            .is_empty()
    );
    assert!(
        store_administration
            .retained_project_shutdown_owners
            .lock()
            .await
            .is_empty(),
        "a clean retry must release retained server shutdown ownership"
    );
}
