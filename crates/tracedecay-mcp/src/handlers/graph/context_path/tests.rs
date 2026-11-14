use std::collections::BTreeSet;
use std::future::{Future, pending, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::task::Poll;
use std::time::Duration;

use tracedecay_contracts::retrieval::catalog::primitive_read_operation;
use tracedecay_contracts::{
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, now_micros,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros};
use tracedecay_runtime_core::native_runtime;

use super::{PathSourceControl, await_path_source};
use crate::tool_context::tests::scope;

fn source_control(deadline: UtcMicros, grant_expires: UtcMicros) -> Arc<PathSourceControl> {
    let operation = primitive_read_operation("context").unwrap().unwrap();
    let scope = scope("context-path-source");
    let actor = ActorId::new("actor.context-path-source").unwrap();
    let cancellation = CancellationSignal::active("cancellation.context-path-source").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.context-path-source").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        actor.clone(),
        UtcMicros(1),
        grant_expires,
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let request = RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.context-path-source").unwrap(),
        Deadline::new(deadline).unwrap(),
        cancellation.context(),
    )
    .unwrap();
    Arc::new(PathSourceControl {
        cancelled: Arc::new(AtomicBool::new(false)),
        cancellation,
        request,
    })
}

fn worker_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("source test worker failed: {error}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_source_returns_before_a_blocked_worker_finishes() {
    let control = source_control(UtcMicros(i64::MAX), UtcMicros(i64::MAX));
    let worker_control = Arc::clone(&control);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let waiter = tokio::spawn(await_path_source(Arc::clone(&control), async move {
        native_runtime::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
            stopped_tx
                .send(worker_control.cancelled.load(Ordering::Acquire))
                .unwrap();
            Ok("source bytes")
        })
        .await
        .map_err(worker_error)?
    }));
    started_rx.await.unwrap();
    assert!(control.cancellation.cancel(now_micros()));
    let outcome = tokio::time::timeout(Duration::from_secs(1), waiter).await;
    // A failure must also release the blocking worker before asserting.
    drop(release_tx);
    let stopped = stopped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let error = outcome
        .expect("cancellation must release the waiter before the worker finishes")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.project_route_context().map(|problem| problem.0),
        Some("context-call-path-source-cancelled"),
    );
    assert!(stopped, "the released worker must observe its stop flag");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_source_waiter_stops_its_worker() {
    let control = source_control(UtcMicros(i64::MAX), UtcMicros(i64::MAX));
    let worker_control = Arc::clone(&control);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (stopped_tx, stopped_rx) = mpsc::channel();
    let waiter = tokio::spawn(await_path_source(control, async move {
        native_runtime::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
            stopped_tx
                .send(worker_control.cancelled.load(Ordering::Acquire))
                .unwrap();
            Ok("source bytes")
        })
        .await
        .map_err(worker_error)?
    }));
    started_rx.await.unwrap();
    waiter.abort();
    let outcome = waiter.await;
    drop(release_tx);
    let stopped = stopped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(outcome.unwrap_err().is_cancelled());
    assert!(stopped, "dropping a read must stop its source worker");
}

#[tokio::test]
async fn source_wait_obeys_the_earlier_request_or_grant_expiry() {
    for grant_expires_first in [false, true] {
        // Finish fixture construction before starting the short admission window.
        let mut control = source_control(UtcMicros(i64::MAX), UtcMicros(i64::MAX));
        let owned_control = Arc::get_mut(&mut control).unwrap();
        let request = &owned_control.request;
        let mut grant = request.grant().clone();
        let soon = UtcMicros(now_micros().0 + 100_000);
        let later = UtcMicros(i64::MAX);
        let (deadline, grant_expires, reason) = if grant_expires_first {
            (later, soon, "context-call-path-source-denied")
        } else {
            (soon, later, "context-call-path-source-timed-out")
        };
        grant.expires_at = grant_expires;
        owned_control.request = RequestContext::new(
            request.actor().clone(),
            request.scope().clone(),
            grant,
            request.request_id().clone(),
            Deadline::new(deadline).unwrap(),
            request.cancellation().clone(),
        )
        .unwrap();
        let mut waiting = pin!(await_path_source(
            Arc::clone(&control),
            pending::<Result<()>>(),
        ));
        poll_fn(|task| {
            assert!(
                waiting.as_mut().poll(task).is_pending(),
                "the source boundary must admit its pending read before expiry",
            );
            Poll::Ready(())
        })
        .await;
        let outcome = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("admission expiry must release a pending source read");
        let error = outcome.unwrap_err();
        assert_eq!(
            error.project_route_context().map(|problem| problem.0),
            Some(reason)
        );
        assert!(control.cancelled.load(Ordering::Acquire));
    }
}

#[tokio::test]
async fn source_completion_cannot_win_a_race_with_cancellation() {
    let control = source_control(UtcMicros(i64::MAX), UtcMicros(i64::MAX));
    let finishing = Arc::clone(&control);
    let error = await_path_source(Arc::clone(&control), async move {
        assert!(finishing.cancellation.cancel(now_micros()));
        Ok("source completed while cancellation was requested")
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.project_route_context().map(|problem| problem.0),
        Some("context-call-path-source-cancelled"),
    );
    assert!(control.cancelled.load(Ordering::Acquire));

    let expired = source_control(UtcMicros(i64::MAX), UtcMicros(now_micros().0 - 1));
    let error = await_path_source(Arc::clone(&expired), pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(
        error.project_route_context().map(|problem| problem.0),
        Some("context-call-path-source-denied"),
    );
    assert!(expired.cancelled.load(Ordering::Acquire));
}
