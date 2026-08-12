use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::settle_in_process_invocation;
use crate::daemon_client::InvocationCancellationPolicy;
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationResponse,
};
use tracedecay_application::{CancellationSignal, clock::now_micros};

fn authoritative_response(request_id: &str) -> DaemonInvocationResponse {
    DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::ResetRequired)
}

fn assert_authoritative_settlement(response: DaemonInvocationResponse) {
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::ResetRequired
        }
    ));
}

#[tokio::test]
async fn in_process_effect_cancellation_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.in-process-effect-cancel";
    let lease = crate::daemon::request_cancellation::register(REQUEST_ID)
        .expect("register invocation cancellation");
    let daemon_cancellation = lease.token();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_by_invocation = Arc::clone(&completed);
    let invocation = tokio::spawn(async move {
        daemon_cancellation.cancelled().await;
        completed_by_invocation.store(true, Ordering::Release);
        authoritative_response(REQUEST_ID)
    });
    let cancellation =
        CancellationSignal::active("cancel.in-process-effect-cancel").expect("cancellation signal");
    let caller_cancellation = cancellation.clone();
    let cancel = tokio::spawn(async move {
        tokio::task::yield_now().await;
        assert!(caller_cancellation.cancel(now_micros()));
    });

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        settle_in_process_invocation(
            REQUEST_ID,
            invocation,
            Duration::from_secs(1),
            cancellation,
            None,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    assert!(completed.load(Ordering::Acquire), "work was not detached");
    cancel.await.expect("cancellation task");
}

#[tokio::test]
async fn in_process_effect_deadline_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.in-process-effect-deadline";
    let lease = crate::daemon::request_cancellation::register(REQUEST_ID)
        .expect("register invocation cancellation");
    let daemon_cancellation = lease.token();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_by_invocation = Arc::clone(&completed);
    let invocation = tokio::spawn(async move {
        daemon_cancellation.cancelled().await;
        completed_by_invocation.store(true, Ordering::Release);
        authoritative_response(REQUEST_ID)
    });

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        settle_in_process_invocation(
            REQUEST_ID,
            invocation,
            Duration::from_millis(10),
            CancellationSignal::active("cancel.in-process-effect-deadline")
                .expect("cancellation signal"),
            None,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    assert!(completed.load(Ordering::Acquire), "work was not detached");
}

#[tokio::test]
async fn in_process_effect_without_settlement_returns_reset_required() {
    const REQUEST_ID: &str = "request.in-process-effect-no-settlement";
    let lease = crate::daemon::request_cancellation::register(REQUEST_ID)
        .expect("register invocation cancellation");
    let invocation = tokio::spawn(std::future::pending::<DaemonInvocationResponse>());

    let response = tokio::time::timeout(
        crate::daemon::DAEMON_TASK_ABORT_DEADLINE + Duration::from_secs(1),
        settle_in_process_invocation(
            REQUEST_ID,
            invocation,
            Duration::from_millis(10),
            CancellationSignal::active("cancel.in-process-effect-no-settlement")
                .expect("cancellation signal"),
            None,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative join is bounded")
    .expect("indeterminate settlement is typed");

    assert_authoritative_settlement(response);
    assert!(lease.token().is_cancelled());
}

#[tokio::test]
async fn in_process_read_observes_admitted_outer_cancellation_after_start() {
    const REQUEST_ID: &str = "request.in-process-admitted-cancellation";
    let registry = crate::daemon::service::project_runtime::ProjectRuntimeRegistryV1::default();
    let project = std::path::PathBuf::from("/projects/in-process-admitted-cancellation");
    registry
        .publish(
            project.clone(),
            Arc::new(1_u32) as Arc<dyn std::any::Any + Send + Sync>,
        )
        .await
        .expect("publish project runtime");
    let admission = registry
        .admit_request(&project, None)
        .expect("admit outer project request");
    let lease = crate::daemon::request_cancellation::register(REQUEST_ID)
        .expect("register invocation cancellation");
    let admitted_cancellation = lease.token();
    let (started, started_rx) = tokio::sync::oneshot::channel::<()>();
    let invocation = tokio::spawn(async move {
        let _admission = admission;
        started.send(()).expect("report invocation start");
        std::future::pending::<DaemonInvocationResponse>().await
    });
    started_rx.await.expect("nested invocation starts");
    let quiescing_registry = registry.clone();
    let quiescing_project = project.clone();
    let quiescence = tokio::spawn(async move {
        quiescing_registry
            .quiesce_roots(&std::collections::BTreeSet::from([quiescing_project]))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !registry.is_root_fenced(&project) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiescence installs its fence");
    assert!(
        !quiescence.is_finished(),
        "the nested request lease must keep quiescence draining"
    );

    admitted_cancellation.cancel();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        settle_in_process_invocation(
            REQUEST_ID,
            invocation,
            Duration::from_secs(10),
            CancellationSignal::active("cancel.in-process-independent")
                .expect("independent cancellation signal"),
            Some(admitted_cancellation),
            InvocationCancellationPolicy::ReadOnly,
        ),
    )
    .await
    .expect("admitted cancellation settles the nested read");

    assert!(matches!(
        response,
        Err(crate::daemon_client::DaemonInvocationError::Cancelled { .. })
    ));
    let guard = tokio::time::timeout(Duration::from_secs(1), quiescence)
        .await
        .expect("nested settlement releases the project lease")
        .expect("quiescence task")
        .expect("quiescence drains the runtime");
    assert!(registry.is_empty().await);
    drop(guard);
    drop(lease);
}
