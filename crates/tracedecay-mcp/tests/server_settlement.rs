use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_mcp::server::{
    DispatchControl, DispatchControlRequest, DispatchSettlement, DispatchToolPolicy,
    RetainedDispatchAuthority, RetainedDispatchOutcome,
};
use tracedecay_runtime_core::native_runtime;

fn deadline_after(duration: Duration) -> tracedecay_contracts::Deadline {
    let micros = i64::try_from(duration.as_micros()).expect("fixture duration");
    tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(
        tracedecay_contracts::clock::now_micros()
            .0
            .saturating_add(micros),
    ))
    .expect("fixture deadline")
}

#[tokio::test]
async fn cancellation_returns_before_a_read_worker_settles_but_shutdown_joins_it() {
    let owner = Arc::new(());
    let authority = Arc::new(RetainedDispatchAuthority::new(Arc::downgrade(&owner)));
    let prepared = authority
        .prepare_control(DispatchControlRequest {
            wire_id: &json!("read-1"),
            connection_scope: "connection-a",
            tool_name: "tracedecay_search",
            pre_cancelled: false,
            caller_deadline: None,
            ceiling: Duration::from_mins(1),
            carried_horizon_micros: Some(60_000_000),
            policy: DispatchToolPolicy {
                live_cancellable: true,
                carries_effect: false,
                canonical_effect_settlement: false,
            },
        })
        .expect("dispatch control");
    let control = prepared.control;
    let cancellation = control.cancellation();
    let worker_started = Arc::new(tokio::sync::Notify::new());
    let worker_release = Arc::new(tokio::sync::Notify::new());
    let worker_finished = Arc::new(AtomicBool::new(false));
    let started = Arc::clone(&worker_started);
    let release = Arc::clone(&worker_release);
    let finished = Arc::clone(&worker_finished);
    let runner_authority = Arc::clone(&authority);
    let runner_control = control.clone();
    let runner = tokio::spawn(async move {
        runner_control
            .run_retained(runner_authority.registry(), async move {
                started.notify_one();
                release.notified().await;
                finished.store(true, Ordering::Release);
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("settled")
            })
            .await
    });

    worker_started.notified().await;
    assert!(cancellation.cancel(tracedecay_contracts::clock::now_micros()));
    let cancelled = runner.await.expect("dispatch task");
    assert_eq!(cancelled.settlement(), DispatchSettlement::Settling);
    assert_eq!(
        cancelled
            .result
            .as_ref()
            .expect_err("cancellation must win")
            .error()
            .project_route_context()
            .map(|context| context.0),
        Some("tool_dispatch_cancelled")
    );
    assert!(!worker_finished.load(Ordering::Acquire));

    let settled = control.settle_cancelled(&cancelled);
    tokio::pin!(settled);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut settled)
            .await
            .is_err(),
        "transport settlement must retain the admitted worker"
    );
    worker_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), settled)
        .await
        .expect("worker completion releases the transport waiter");
    authority.shutdown().await;
    assert!(worker_finished.load(Ordering::Acquire));
}

/// A Tokio runtime without a timer driver: a dispatch deadline still timed by
/// Tokio panics here instead of passing on the wrong clock.
fn runtime_without_tokio_timers() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .expect("tokio runtime")
}

/// Fails a deadline that never fires instead of hanging the test.
async fn within<F: Future>(limit: Duration, future: F) -> Option<F::Output> {
    tokio::select! {
        output = future => Some(output),
        _ = native_runtime::sleep_until(Instant::now() + limit) => None,
    }
}

fn deadline_code<T>(outcome: &RetainedDispatchOutcome<T>) -> Option<&str> {
    outcome
        .result
        .as_ref()
        .err()
        .and_then(|failure| failure.error().project_route_context())
        .map(|context| context.0)
}

const DISPATCH_DEADLINE: Duration = Duration::from_millis(100);
const WAIT_LIMIT: Duration = Duration::from_secs(10);
/// The deadline is carried in wall-clock micros, so the monotonic deadline
/// may land up to a rounding step before `started + DISPATCH_DEADLINE`.
const ROUNDING: Duration = Duration::from_millis(1);

#[test]
fn canonical_effect_waits_past_its_deadline_for_the_authoritative_result() {
    runtime_without_tokio_timers().block_on(async {
        let owner = Arc::new(());
        let authority = RetainedDispatchAuthority::new(Arc::downgrade(&owner));
        let cancellation = tracedecay_contracts::CancellationSignal::active("effect.deadline")
            .expect("cancellation");
        let started = Instant::now();
        let control = DispatchControl::new(
            "tracedecay_configuration_set",
            deadline_after(DISPATCH_DEADLINE),
            cancellation.clone(),
            DispatchToolPolicy {
                live_cancellable: false,
                carries_effect: true,
                canonical_effect_settlement: true,
            },
        )
        .expect("dispatch control");
        // The worker answers only after the dispatch deadline, so the
        // deadline branch is the one that has to wait for its answer.
        let worker_answers_at = started + 2 * DISPATCH_DEADLINE;
        let outcome = within(
            WAIT_LIMIT,
            control.run_retained(authority.registry(), async move {
                native_runtime::sleep_until(worker_answers_at)
                    .await
                    .expect("native timer");
                Ok::<_, TraceDecayError>("canonical")
            }),
        )
        .await
        .expect("the canonical result must arrive");

        assert_eq!(
            outcome.result.as_ref().expect("canonical result"),
            &"canonical",
            "the transport must not replace an admitted effect's canonical terminal"
        );
        assert_eq!(outcome.settlement(), DispatchSettlement::Joined);
        assert!(!cancellation.is_cancelled());
        authority.shutdown().await;
    });
}

#[test]
fn deadline_answers_for_a_worker_that_never_settles() {
    runtime_without_tokio_timers().block_on(async {
        let owner = Arc::new(());
        let authority = RetainedDispatchAuthority::new(Arc::downgrade(&owner));
        let cancellation = tracedecay_contracts::CancellationSignal::active("read.deadline")
            .expect("cancellation");
        let started = Instant::now();
        let control = DispatchControl::new(
            "tracedecay_search",
            deadline_after(DISPATCH_DEADLINE),
            cancellation.clone(),
            DispatchToolPolicy {
                live_cancellable: false,
                carries_effect: false,
                canonical_effect_settlement: false,
            },
        )
        .expect("dispatch control");
        let release = Arc::new(tokio::sync::Notify::new());
        let worker_release = Arc::clone(&release);
        let outcome = within(
            WAIT_LIMIT,
            control.run_retained(authority.registry(), async move {
                worker_release.notified().await;
                Ok::<_, TraceDecayError>("late")
            }),
        )
        .await
        .expect("the deadline must answer for a stuck worker");
        let elapsed = started.elapsed();

        assert_eq!(
            deadline_code(&outcome),
            Some("tool_dispatch_deadline_exceeded")
        );
        assert!(
            cancellation.is_cancelled(),
            "the deadline cancels the worker"
        );
        assert!(
            elapsed >= DISPATCH_DEADLINE - ROUNDING,
            "the deadline must not fire early; elapsed={elapsed:?}"
        );
        release.notify_one();
        authority.shutdown().await;
    });
}

#[test]
fn cancelled_settlement_stops_waiting_at_the_deadline() {
    runtime_without_tokio_timers().block_on(async {
        let owner = Arc::new(());
        let authority = RetainedDispatchAuthority::new(Arc::downgrade(&owner));
        let cancellation = tracedecay_contracts::CancellationSignal::active("read.cancelled")
            .expect("cancellation");
        assert!(cancellation.cancel(tracedecay_contracts::clock::now_micros()));
        let started = Instant::now();
        let control = DispatchControl::new(
            "tracedecay_search",
            deadline_after(DISPATCH_DEADLINE),
            cancellation,
            DispatchToolPolicy {
                live_cancellable: true,
                carries_effect: false,
                canonical_effect_settlement: false,
            },
        )
        .expect("dispatch control");
        let release = Arc::new(tokio::sync::Notify::new());
        let worker_finished = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let finished = Arc::clone(&worker_finished);
        let cancelled = control
            .run_retained(authority.registry(), async move {
                // Ignores its cancellation signal.
                worker_release.notified().await;
                finished.store(true, Ordering::Release);
                Ok::<_, TraceDecayError>("ignored")
            })
            .await;
        assert_eq!(deadline_code(&cancelled), Some("tool_dispatch_cancelled"));

        within(WAIT_LIMIT, control.settle_cancelled(&cancelled))
            .await
            .expect("a worker that ignores cancellation must not hold the reply past the deadline");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= DISPATCH_DEADLINE - ROUNDING,
            "the settlement wait must last until the deadline; elapsed={elapsed:?}"
        );
        assert!(!worker_finished.load(Ordering::Acquire));
        release.notify_one();
        authority.shutdown().await;
        assert!(worker_finished.load(Ordering::Acquire));
    });
}
