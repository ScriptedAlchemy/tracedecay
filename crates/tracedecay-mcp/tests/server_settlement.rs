use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tracedecay_mcp::server::{
    DispatchControlRequest, DispatchSettlement, DispatchToolPolicy, RetainedDispatchAuthority,
};

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
    let cancellation = prepared.control.cancellation();
    let worker_started = Arc::new(tokio::sync::Notify::new());
    let worker_release = Arc::new(tokio::sync::Notify::new());
    let worker_finished = Arc::new(AtomicBool::new(false));
    let started = Arc::clone(&worker_started);
    let release = Arc::clone(&worker_release);
    let finished = Arc::clone(&worker_finished);
    let runner_authority = Arc::clone(&authority);
    let runner = tokio::spawn(async move {
        prepared
            .control
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
            .expect_err("cancellation must win")
            .error()
            .project_route_context()
            .map(|context| context.0),
        Some("tool_dispatch_cancelled")
    );
    assert!(!worker_finished.load(Ordering::Acquire));

    worker_release.notify_one();
    authority.shutdown().await;
    assert!(worker_finished.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn canonical_effect_waits_for_its_authoritative_deadline_result() {
    let owner = Arc::new(());
    let authority = Arc::new(RetainedDispatchAuthority::new(Arc::downgrade(&owner)));
    let cancellation =
        tracedecay_contracts::CancellationSignal::active("effect.deadline").expect("cancellation");
    let control = tracedecay_mcp::server::DispatchControl::new(
        "tracedecay_configuration_set",
        deadline_after(Duration::from_secs(1)),
        cancellation,
        DispatchToolPolicy {
            live_cancellable: false,
            carries_effect: true,
            canonical_effect_settlement: true,
        },
    )
    .expect("dispatch control");
    let worker_started = Arc::new(tokio::sync::Notify::new());
    let worker_release = Arc::new(tokio::sync::Notify::new());
    let started = Arc::clone(&worker_started);
    let release = Arc::clone(&worker_release);
    let runner_authority = Arc::clone(&authority);
    let runner = tokio::spawn(async move {
        control
            .run_retained(runner_authority.registry(), async move {
                started.notify_one();
                release.notified().await;
                Ok::<_, tracedecay_domain::errors::TraceDecayError>("canonical")
            })
            .await
    });

    worker_started.notified().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(
        !runner.is_finished(),
        "the transport must not replace an admitted effect's canonical terminal"
    );
    worker_release.notify_one();
    let outcome = runner.await.expect("dispatch task");
    assert_eq!(
        outcome.result.as_ref().expect("canonical result"),
        &"canonical"
    );
    assert_eq!(outcome.settlement(), DispatchSettlement::Joined);
    authority.shutdown().await;
}
