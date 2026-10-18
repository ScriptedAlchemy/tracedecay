use std::sync::Arc;

use tracedecay_runtime_core::native_runtime::{self, NativeTask};

use super::{
    DashboardTaskCompletion, RunningDashboard, get_manager, shutdown_dashboard_for_until,
    shutdown_dashboard_until,
};

fn native_task(work: impl Future<Output = ()> + Send + 'static) -> Arc<NativeTask<()>> {
    Arc::new(NativeTask::spawn(work).expect("native dashboard task"))
}

/// The dashboard manager is process global, and these tests assert on its
/// whole contents, so they must not observe each other's entries.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn shutdown_deadline_aborts_joins_and_clears_dashboard_task() {
    let _guard = TEST_LOCK.lock().await;
    let project_root = std::path::PathBuf::from("/dashboard-test/project");
    let mut manager = get_manager().lock().await;
    assert!(
        manager.is_empty(),
        "dashboard test requires an idle manager"
    );
    let (shutdown, _shutdown_requested) = tokio::sync::oneshot::channel();
    let completed = Arc::new(tokio::sync::Semaphore::new(0));
    let completion = DashboardTaskCompletion(Arc::clone(&completed));
    let task = native_task(async move {
        let _completion = completion;
        std::future::pending::<()>().await;
    });
    manager.insert(
        project_root.clone(),
        RunningDashboard {
            url: "http://127.0.0.1:0/".to_owned(),
            addr: "127.0.0.1:0".parse().expect("socket addr"),
            shutdown: Some(shutdown),
            task,
            completed,
        },
    );
    drop(manager);

    let error = shutdown_dashboard_until(std::time::Instant::now())
        .await
        .expect_err("expired dashboard shutdown must report its abort");

    assert!(error.to_string().contains("was aborted"));
    assert!(get_manager().lock().await.is_empty());
}

/// A serving task signals its completion by dropping its
/// [`DashboardTaskCompletion`], which happens strictly before the runtime
/// marks the task finished. A stop that starts inside that window
/// must still observe the completion: the server is already down, so
/// reporting a missed deadline would fail a stop that in fact succeeded.
#[tokio::test]
async fn shutdown_observes_a_completion_signalled_before_the_stop_began() {
    let _guard = TEST_LOCK.lock().await;
    let project_root = std::path::PathBuf::from("/dashboard-test/completed-before-stop");
    let (shutdown, _shutdown_requested) = tokio::sync::oneshot::channel();
    let completed = Arc::new(tokio::sync::Semaphore::new(0));
    let completion = DashboardTaskCompletion(Arc::clone(&completed));
    let (signalled, observe_signalled) = tokio::sync::oneshot::channel();
    let task = native_task(async move {
        drop(completion);
        let _ = signalled.send(());
        // Holds the task open well past the stop's deadline so that a stop
        // which waits on the task's join instead of the signal
        // reports a deadline miss.
        let _ = native_runtime::sleep_until(
            std::time::Instant::now() + std::time::Duration::from_millis(300),
        )
        .await;
    });
    get_manager().lock().await.insert(
        project_root.clone(),
        RunningDashboard {
            url: "http://127.0.0.1:0/".to_owned(),
            addr: "127.0.0.1:0".parse().expect("socket addr"),
            shutdown: Some(shutdown),
            task,
            completed,
        },
    );
    observe_signalled
        .await
        .expect("serving task signals its completion");

    shutdown_dashboard_for_until(
        &project_root,
        std::time::Instant::now() + std::time::Duration::from_millis(50),
    )
    .await
    .expect("a stop that begins after the completion signal must not miss it");

    assert!(get_manager().lock().await.is_empty());
}

/// The daemon coordinator drops the hosted-dashboard join at its phase
/// deadline. A serving task that ignores its shutdown request must be
/// aborted then, not left serving past the shutdown receipt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_shutdown_owner_aborts_the_dashboard_task() {
    let _guard = TEST_LOCK.lock().await;
    let project_root = std::path::PathBuf::from("/dashboard-test/timed-out-owner");
    let (shutdown, _shutdown_requested) = tokio::sync::oneshot::channel();
    let completed = Arc::new(tokio::sync::Semaphore::new(0));
    let completion = DashboardTaskCompletion(Arc::clone(&completed));
    let (alive, task_dropped) = std::sync::mpsc::channel::<()>();
    let task = native_task(async move {
        let _completion = completion;
        let _alive = alive;
        std::future::pending::<()>().await;
    });
    get_manager().lock().await.insert(
        project_root,
        RunningDashboard {
            url: "http://127.0.0.1:0/".to_owned(),
            addr: "127.0.0.1:0".parse().expect("socket addr"),
            shutdown: Some(shutdown),
            task,
            completed,
        },
    );

    let receipt = tracedecay_daemon_service::shutdown::join_shutdown_owners(
        std::time::Instant::now() + std::time::Duration::from_millis(100),
        vec![
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
                "hosted_dashboard",
                || {},
                |_| async { super::shutdown_dashboard().await },
            ),
        ],
    )
    .await;

    assert_eq!(
        receipt.owners[0].status,
        tracedecay_daemon_service::shutdown::ShutdownStatus::TimedOut
    );
    assert_eq!(
        task_dropped.recv_timeout(std::time::Duration::from_secs(5)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        "a timed-out owner must not leave the dashboard task it was joining running"
    );
    let _ = shutdown_dashboard_until(std::time::Instant::now() + std::time::Duration::from_secs(5))
        .await;
    assert!(get_manager().lock().await.is_empty());
}

/// The session resolution for a dashboard of a still-opening project runs
/// where no Tokio runtime exists and publishes its terminal answer.
#[test]
fn opening_project_sessions_resolve_without_a_tokio_runtime() {
    let hazards = crate::test_support::native_runtime_hazards();
    let unavailable = tracedecay_runtime_core::native_runtime::block_on(async {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let resolver = crate::mcp::server::install_retained_project_server_resolver(|_| {
            Box::pin(async { Ok(None) })
        });
        let mut sessions = super::opening_project_sessions(
            resolver,
            tracedecay_daemon_service::ProjectRuntimeRegistryV1::default(),
            std::path::PathBuf::from("/dashboard-test/never-published"),
        );
        let resolution = sessions
            .wait_for(|resolution| {
                !matches!(
                    resolution,
                    tracedecay_dashboard_api::DashboardSessionResolutionV1::Opening
                )
            })
            .await
            .expect("resolution is published");
        matches!(
            *resolution,
            tracedecay_dashboard_api::DashboardSessionResolutionV1::Unavailable
        )
    })
    .expect("native runtime");
    assert!(
        unavailable,
        "an unpublished project resolves as unavailable"
    );
    assert_eq!(crate::test_support::native_runtime_hazards(), hazards);
}
