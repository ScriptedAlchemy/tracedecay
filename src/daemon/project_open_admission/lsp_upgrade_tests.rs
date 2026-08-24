use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_runtime_core::cancellation::CancellationToken;

use super::{ProjectOpenTaskClaim, ProjectOpenTasks, ProjectOpenWaitOutcome, ProjectRouteKey};
use crate::errors::TraceDecayError;

fn route(project_path: &str, scope_prefix: Option<&str>) -> ProjectRouteKey {
    ProjectRouteKey {
        profile_root: PathBuf::from("/profile"),
        global_db_path: PathBuf::from("/profile/global.db"),
        project_path: PathBuf::from(project_path),
        scope_prefix: scope_prefix.map(str::to_owned),
    }
}

fn open_deadline() -> tracedecay_application::Deadline {
    tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
        tracedecay_application::clock::now_micros()
            .0
            .saturating_add(5_000_000),
    ))
    .expect("valid test deadline")
}

#[tokio::test]
async fn lsp_wait_stays_pending_after_core_publication_until_full_open_finishes() {
    let tasks = ProjectOpenTasks::default();
    let route = route("/workspace", None);
    let full_open = Arc::new(tokio::sync::Notify::new());
    let release = Arc::clone(&full_open);
    assert!(matches!(
        tasks
            .start(route.clone(), async move {
                release.notified().await;
                Ok(())
            })
            .await,
        ProjectOpenTaskClaim::InFlight(_)
    ));
    let cancellation = CancellationToken::new();
    let deadline = open_deadline();
    let wait = tasks.wait_for_lsp_upgrade(&route, &deadline, &cancellation);
    tokio::pin!(wait);
    tokio::select! {
        _outcome = &mut wait => panic!("full-open wait completed before the tracked task"),
        () = tokio::time::sleep(Duration::from_millis(10)) => {}
    }
    full_open.notify_one();
    assert!(matches!(wait.await, ProjectOpenWaitOutcome::Completed));
}

#[tokio::test]
async fn lsp_wait_observes_cancellation_and_deadline_before_full_open() {
    let tasks = ProjectOpenTasks::default();
    let route = route("/workspace", None);
    let release = Arc::new(tokio::sync::Notify::new());
    let open_release = Arc::clone(&release);
    assert!(matches!(
        tasks
            .start(route.clone(), async move {
                open_release.notified().await;
                Ok(())
            })
            .await,
        ProjectOpenTaskClaim::InFlight(_)
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancellation_deadline = open_deadline();
    assert!(matches!(
        tasks
            .wait_for_lsp_upgrade(&route, &cancellation_deadline, &cancellation)
            .await,
        ProjectOpenWaitOutcome::Cancelled
    ));

    let expired =
        tracedecay_application::Deadline::new(tracedecay_application::clock::now_micros())
            .expect("valid expired test deadline");
    assert!(matches!(
        tasks
            .wait_for_lsp_upgrade(&route, &expired, &CancellationToken::new())
            .await,
        ProjectOpenWaitOutcome::TimedOut
    ));
    release.notify_one();
}

#[tokio::test]
async fn lsp_wait_requires_exact_route_identity_for_linked_worktrees() {
    let tasks = ProjectOpenTasks::default();
    let base_route = route("/workspace", None);
    let linked_route = route("/workspace", Some("packages/api"));
    let release = Arc::new(tokio::sync::Notify::new());
    let open_release = Arc::clone(&release);
    assert!(matches!(
        tasks
            .start(base_route.clone(), async move {
                open_release.notified().await;
                Ok(())
            })
            .await,
        ProjectOpenTaskClaim::InFlight(_)
    ));

    let linked_deadline = open_deadline();
    assert!(matches!(
        tasks
            .wait_for_lsp_upgrade(&linked_route, &linked_deadline, &CancellationToken::new(),)
            .await,
        ProjectOpenWaitOutcome::NotTracked
    ));
    release.notify_one();
    let completion_deadline = open_deadline();
    assert!(matches!(
        tasks
            .wait_for_lsp_upgrade(&base_route, &completion_deadline, &CancellationToken::new(),)
            .await,
        ProjectOpenWaitOutcome::Completed
    ));
}

#[tokio::test]
async fn lsp_wait_preserves_a_typed_full_open_failure() {
    let tasks = ProjectOpenTasks::default();
    let route = route("/workspace", None);
    assert!(matches!(
        tasks
            .start(route.clone(), async {
                Err(TraceDecayError::ResetRequired {
                    authority: "lsp".to_owned(),
                    reason: "owner registration was rejected".to_owned(),
                })
            })
            .await,
        ProjectOpenTaskClaim::InFlight(_)
    ));

    let failure_deadline = open_deadline();
    let outcome = tasks
        .wait_for_lsp_upgrade(&route, &failure_deadline, &CancellationToken::new())
        .await;
    assert!(matches!(
        outcome,
        ProjectOpenWaitOutcome::Failed(TraceDecayError::ResetRequired {
            authority,
            reason,
        }) if authority == "lsp" && reason == "owner registration was rejected"
    ));
}
