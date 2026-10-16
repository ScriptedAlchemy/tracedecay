use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{invocation_is_native_integration_operation, settle_in_process_invocation};
use tracedecay_contracts::{CancellationSignal, clock::now_micros};
use tracedecay_daemon_protocol::InvocationCancellationPolicy;
use tracedecay_daemon_protocol::{
    DAEMON_TOOL_RESPONSE_GRACE, DaemonInvocationOutcome, DaemonInvocationProblem,
    DaemonInvocationResponse,
};
use tracedecay_daemon_service::{DaemonInvocationOperation, RequestCancellationRegistryV1};
use tracedecay_runtime_core::native_runtime::NativeTask;

#[test]
fn native_worktree_operations_request_the_native_integration_owner() {
    for operation in [
        DaemonInvocationOperation::NativeIntegrationWorktreeInventory,
        DaemonInvocationOperation::NativeIntegrationWorktreeInspect,
        DaemonInvocationOperation::NativeIntegrationWorktreeConfirm,
        DaemonInvocationOperation::NativeIntegrationWorktreeRemove,
        DaemonInvocationOperation::NativeIntegrationWorktreeReconcile,
    ] {
        assert!(invocation_is_native_integration_operation(operation));
    }
}

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
    let request_cancellations = RequestCancellationRegistryV1::default();
    let lease = request_cancellations
        .register(REQUEST_ID)
        .expect("register invocation cancellation");
    let daemon_cancellation = lease.token();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_by_invocation = Arc::clone(&completed);
    let invocation = NativeTask::spawn(async move {
        daemon_cancellation.cancelled().await;
        completed_by_invocation.store(true, Ordering::Release);
        authoritative_response(REQUEST_ID)
    })
    .expect("native invocation task");
    let cancellation =
        CancellationSignal::active("cancel.in-process-effect-cancel").expect("cancellation signal");
    let caller_cancellation = cancellation.clone();
    let cancel = tokio::spawn(async move {
        tokio::task::yield_now().await;
        assert!(caller_cancellation.cancel(now_micros()));
    });

    // The deadline sits past any test run, so only the caller's cancellation
    // can request the daemon cancel this settlement waits on.
    let response = settle_in_process_invocation(
        &request_cancellations,
        REQUEST_ID,
        invocation,
        Duration::from_hours(24),
        cancellation,
        None,
        InvocationCancellationPolicy::AuthoritativeEffect,
        DAEMON_TOOL_RESPONSE_GRACE,
    )
    .await
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    assert!(completed.load(Ordering::Acquire), "work was not detached");
    cancel.await.expect("cancellation task");
}

#[tokio::test]
async fn in_process_effect_deadline_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.in-process-effect-deadline";
    let request_cancellations = RequestCancellationRegistryV1::default();
    let lease = request_cancellations
        .register(REQUEST_ID)
        .expect("register invocation cancellation");
    let daemon_cancellation = lease.token();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_by_invocation = Arc::clone(&completed);
    let invocation = NativeTask::spawn(async move {
        daemon_cancellation.cancelled().await;
        completed_by_invocation.store(true, Ordering::Release);
        authoritative_response(REQUEST_ID)
    })
    .expect("native invocation task");

    let response = settle_in_process_invocation(
        &request_cancellations,
        REQUEST_ID,
        invocation,
        Duration::from_millis(10),
        CancellationSignal::active("cancel.in-process-effect-deadline")
            .expect("cancellation signal"),
        None,
        InvocationCancellationPolicy::AuthoritativeEffect,
        DAEMON_TOOL_RESPONSE_GRACE,
    )
    .await
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    assert!(completed.load(Ordering::Acquire), "work was not detached");
}

#[tokio::test]
async fn in_process_effect_without_settlement_returns_reset_required() {
    const REMAINING: Duration = Duration::from_millis(10);
    const RESPONSE_GRACE: Duration = Duration::from_millis(100);
    const REQUEST_ID: &str = "request.in-process-effect-no-settlement";
    let request_cancellations = RequestCancellationRegistryV1::default();
    let lease = request_cancellations
        .register(REQUEST_ID)
        .expect("register invocation cancellation");
    let invocation = NativeTask::spawn(std::future::pending::<DaemonInvocationResponse>())
        .expect("native invocation task");

    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        settle_in_process_invocation(
            &request_cancellations,
            REQUEST_ID,
            invocation,
            REMAINING,
            CancellationSignal::active("cancel.in-process-effect-no-settlement")
                .expect("cancellation signal"),
            None,
            InvocationCancellationPolicy::AuthoritativeEffect,
            RESPONSE_GRACE,
        ),
    )
    .await
    .expect("authoritative join did not terminate after its response grace")
    .expect("indeterminate settlement is typed");
    let waited = started.elapsed();

    assert!(
        waited >= REMAINING + RESPONSE_GRACE,
        "authoritative join must wait out its deadline and response grace, waited {waited:?}"
    );
    assert_authoritative_settlement(response);
    assert!(lease.token().is_cancelled());
}

#[tokio::test]
async fn in_process_read_observes_admitted_outer_cancellation_after_start() {
    const REQUEST_ID: &str = "request.in-process-admitted-cancellation";
    let registry = tracedecay_daemon_service::ProjectRuntimeRegistryV1::default();
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
    let request_cancellations = RequestCancellationRegistryV1::default();
    let lease = request_cancellations
        .register(REQUEST_ID)
        .expect("register invocation cancellation");
    let admitted_cancellation = lease.token();
    let invocation_cancellation = admitted_cancellation.clone();
    let (started, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (alive, ended) = tokio::sync::oneshot::channel::<()>();
    let invocation = NativeTask::spawn(async move {
        let _alive = alive;
        started.send(()).expect("report invocation start");
        // The read gives up its project lease once it observes the admitted
        // cancellation, but never returns: only settlement ends it.
        invocation_cancellation.cancelled().await;
        drop(admission);
        std::future::pending::<DaemonInvocationResponse>().await
    })
    .expect("native invocation task");
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
    // Without the admitted cancellation the read would settle `TimedOut`
    // only once the ten second deadline passed.
    let response = settle_in_process_invocation(
        &request_cancellations,
        REQUEST_ID,
        invocation,
        Duration::from_secs(10),
        CancellationSignal::active("cancel.in-process-independent")
            .expect("independent cancellation signal"),
        Some(admitted_cancellation),
        InvocationCancellationPolicy::ReadOnly,
        DAEMON_TOOL_RESPONSE_GRACE,
    )
    .await;

    assert!(matches!(
        response,
        Err(tracedecay_daemon_protocol::DaemonInvocationError::Cancelled { .. })
    ));
    assert!(
        ended.await.is_err(),
        "settlement aborts the read that never returns"
    );
    let guard = quiescence
        .await
        .expect("quiescence task")
        .expect("the released lease lets quiescence drain");
    assert!(registry.is_empty());
    drop(guard);
    drop(lease);
}
