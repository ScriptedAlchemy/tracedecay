use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::{SessionId, UtcMicros};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_session_temporal_store::SessionTemporalStore;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionRefreshStore, SessionTemporalProjectionBatchV1,
};

use super::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshEffect, SessionTemporalRefreshPolicy,
    zero_refresh_coverage,
};
use super::registry::SessionTemporalRefreshPassReport;
use super::wake::{SessionTemporalRefreshRetryClass, SessionTemporalRefreshWakeState};
use super::worker::{apply_refresh_effect, run_session_temporal_refresh_pass};

async fn begin_empty_refreshes(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    names: impl IntoIterator<Item = &'static str>,
) {
    for name in names {
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                SessionId::new(name).expect("session id"),
                SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
            ))
            .await
            .expect("running recovery");
    }
}

#[tokio::test]
async fn durable_recovery_progresses_before_another_discovery_scan() {
    let harness = RegisteredGlobalDbHarness::open("refresh-durable-recovery-first").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["durable-recovery-first"]).await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());

    let report = run_session_temporal_refresh_pass(
        &harness.registered,
        &state,
        &CanonicalSessionTemporalProjector,
        SessionTemporalRefreshPolicy {
            max_operations_per_pass: 1,
            ..SessionTemporalRefreshPolicy::default()
        },
    )
    .await;

    assert_eq!(report.projected_batches, 1);
    assert_eq!(report.retryable_errors, 0);
    assert!(
        report.saturated,
        "deferring discovery must schedule a follow-up pass"
    );
}

#[tokio::test]
async fn retryable_recovery_stops_the_pass_and_preserves_unattempted_work() {
    let harness = RegisteredGlobalDbHarness::open("refresh-retry-stops-pass").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["retry-first", "retry-second"]).await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());

    let report = run_session_temporal_refresh_pass(
        &harness.registered,
        &state,
        &CanonicalSessionTemporalProjector,
        SessionTemporalRefreshPolicy {
            max_operations_per_pass: 2,
            operation_deadline: Duration::ZERO,
            ..SessionTemporalRefreshPolicy::default()
        },
    )
    .await;

    assert_eq!(report.deadline_errors, 1);
    assert_eq!(
        report.retry_class,
        Some(SessionTemporalRefreshRetryClass::Deadline)
    );
    assert_eq!(report.backlog, Some(2));
    assert_eq!(state.pending_recovery_operations().len(), 1);
}

#[tokio::test]
async fn refused_projection_progress_retires_the_refresh_instead_of_retrying() {
    let harness = RegisteredGlobalDbHarness::open("refresh-refused-progress-retires").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["refused-progress"]).await;
    let recovery = store
        .running_session_refreshes()
        .await
        .expect("recoveries")
        .pop()
        .expect("one running recovery");
    let state = SessionTemporalRefreshWakeState::default();
    let mut report = SessionTemporalRefreshPassReport::default();

    // Progress that claims a second committed batch while submitting the
    // first one. The durable contract refuses it, and every later pass would
    // hand the projector the same state and rebuild the same refused row.
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
        zero_refresh_coverage(),
        2,
        0,
        UtcMicros(1),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .expect("batch")
    .with_checkpoint(0, 0, 0)
    .expect("checkpoint");

    apply_refresh_effect(
        &store,
        &state,
        &recovery,
        SessionTemporalRefreshEffect::Projection { progress, batch },
        &mut report,
    )
    .await;

    assert_eq!(
        report.failed, 1,
        "a refused progress row must retire the refresh, not stay running"
    );
    assert_eq!(report.retryable_errors, 0);
    assert_eq!(report.terminal_errors, 0);
    assert!(
        store
            .running_session_refreshes()
            .await
            .expect("recoveries")
            .is_empty(),
        "the retired refresh must not be rediscovered"
    );
}
