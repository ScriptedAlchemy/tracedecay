use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::SessionId;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_session_temporal_store::SessionTemporalStore;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshStore,
};

use super::projector::{CanonicalSessionTemporalProjector, SessionTemporalRefreshPolicy};
use super::wake::{SessionTemporalRefreshRetryClass, SessionTemporalRefreshWakeState};
use super::worker::run_session_temporal_refresh_pass;

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
