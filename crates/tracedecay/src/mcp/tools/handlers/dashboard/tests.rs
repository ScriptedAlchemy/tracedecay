use std::sync::Arc;

use super::{
    DashboardTaskCompletion, RunningDashboard, get_manager, shutdown_dashboard_for_until,
    shutdown_dashboard_until,
};

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
    let task = tokio::spawn(async move {
        let _completion = completion;
        std::future::pending::<tracedecay_domain::errors::Result<()>>().await
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

    let error = shutdown_dashboard_until(tokio::time::Instant::now())
        .await
        .expect_err("expired dashboard shutdown must report its abort");

    assert!(error.to_string().contains("was aborted"));
    assert!(get_manager().lock().await.is_empty());
}

/// A serving task signals its completion by dropping its
/// [`DashboardTaskCompletion`], which happens strictly before the runtime
/// marks the `JoinHandle` finished. A stop that starts inside that window
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
    let task = tokio::spawn(async move {
        drop(completion);
        let _ = signalled.send(());
        // Holds the task open well past the stop's deadline so that a stop
        // which waits on the `JoinHandle` transition instead of the signal
        // reports a deadline miss.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(())
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
        tokio::time::Instant::now() + std::time::Duration::from_millis(50),
    )
    .await
    .expect("a stop that begins after the completion signal must not miss it");

    assert!(get_manager().lock().await.is_empty());
}
