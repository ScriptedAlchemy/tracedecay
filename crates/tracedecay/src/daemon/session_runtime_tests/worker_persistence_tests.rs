use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use tracedecay_domain::{SessionId, TemporalCoverageCountsV1, UtcMicros};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionRefreshStore, SessionTemporalProjectionBatchV1,
};

use crate::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_session_runtime::session_sync::test_harness::{
    SessionTemporalRefreshPassReport, SessionTemporalRefreshWakeState, apply_refresh_effect,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::projector::SessionTemporalRefreshEffect;
use tracedecay_session_temporal_store::SessionRefreshRestartStateV1;

#[tokio::test]
async fn cancelled_worker_control_prevents_projection_batch_persistence() {
    let temp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path())
        .await
        .unwrap();
    let store = runtime
        .session_temporal_store_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = SessionId::new("session.scheduler.cancelled-persistence").unwrap();
    let started = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(0, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = SessionRefreshProgressV1::new(
        started.operation_id().clone(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(0, 0).unwrap(),
        TemporalCoverageCountsV1 {
            visible: 0,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
        1,
        0,
        UtcMicros(
            i64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros(),
            )
            .unwrap(),
        ),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap()
    .with_checkpoint(0, 0, 0)
    .unwrap();
    let state = SessionTemporalRefreshWakeState::default();
    state.cancel();
    let mut report = SessionTemporalRefreshPassReport::default();

    apply_refresh_effect(
        &store,
        &state,
        &recovery,
        SessionTemporalRefreshEffect::Projection { progress, batch },
        &mut report,
    )
    .await;

    assert_eq!(report.projected_batches, 0);
    assert_eq!(report.terminal_errors, 1);
    assert_eq!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}
