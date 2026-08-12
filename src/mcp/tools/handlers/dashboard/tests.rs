use std::sync::Arc;

use super::{DashboardTaskCompletion, RunningDashboard, get_manager, shutdown_dashboard_until};

#[tokio::test]
async fn shutdown_deadline_aborts_joins_and_clears_dashboard_task() {
    let mut manager = get_manager().lock().await;
    assert!(manager.is_none(), "dashboard test requires an idle manager");
    let (shutdown, _shutdown_requested) = tokio::sync::oneshot::channel();
    let completed = Arc::new(tokio::sync::Notify::new());
    let completion = DashboardTaskCompletion(Arc::clone(&completed));
    let task = tokio::spawn(async move {
        let _completion = completion;
        std::future::pending::<crate::errors::Result<()>>().await
    });
    *manager = Some(RunningDashboard {
        url: "http://127.0.0.1:0/".to_owned(),
        shutdown: Some(shutdown),
        task,
        completed,
    });
    drop(manager);

    let error = shutdown_dashboard_until(tokio::time::Instant::now())
        .await
        .expect_err("expired dashboard shutdown must report its abort");

    assert!(error.to_string().contains("was aborted"));
    assert!(get_manager().lock().await.is_none());
}
