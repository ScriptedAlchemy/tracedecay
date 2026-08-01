use super::projector::*;
use super::registry::*;
use super::wake::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, TemporalCoverageCountsV1, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshReceiptRequestV1,
    SessionRefreshStore, SessionRefreshTerminalStateV1, SessionTemporalProjectionBatchV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::global_db::RegisteredGlobalDb;
use crate::store::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};

use super::SessionTemporalRefreshTestAuthority;

async fn registered_test_database(
    temp: &TempDir,
    label: &str,
    scope: HostAdmissionScope,
) -> SessionTemporalRefreshTestAuthority {
    let profile_root = temp.path().join(format!("{label}-profile"));
    let runtime = match scope {
        HostAdmissionScope::Profile => HostAdmissionTestRuntimeV1::profile(profile_root)
            .await
            .unwrap(),
        HostAdmissionScope::Project => HostAdmissionTestRuntimeV1::project(
            profile_root,
            temp.path().join(format!("{label}-project")),
            tracedecay_domain::ProjectId::new(format!("project.refresh-{label}")).unwrap(),
        )
        .await
        .unwrap(),
    };
    runtime
        .into_session_temporal_refresh_test_authority(scope)
        .expect("registered temporal test database")
}

fn sanitization_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.refresh-scheduler-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn canonical_observation(session_id: &SessionId, ordinal: u64, text: &str) -> DurableObservationV1 {
    let provider = ProviderId::new(format!("cursor-refresh-{ordinal}")).unwrap();
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let record_id = ObservationId::new(format!("record.refresh-scheduler.{ordinal}")).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id.clone()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": text}),
            model: Some("model.fixture".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    DurableObservationV1::new(
        identity,
        sanitization_receipt(&format!("receipt.refresh-scheduler.{ordinal}"), &payload),
        RetentionClass::new("retention.refresh-scheduler-test").unwrap(),
        payload,
    )
    .unwrap()
}

fn anchored_write(observation: DurableObservationV1) -> AnchoredObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, None, next_cursor).unwrap();
    let generation = ProjectionGenerationId::new("projection.refresh-scheduler-test.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "refresh-scheduler")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, generation).unwrap()
}

async fn admit_canonical_effect(
    db: &RegisteredGlobalDb,
    session_id: &SessionId,
    ordinal: u64,
    text: &str,
) {
    let store = crate::store::GlobalDbObservationStore::with_runtime(db.runtime(), db.authority());
    store
        .persist_observation(anchored_write(canonical_observation(
            session_id, ordinal, text,
        )))
        .await
        .unwrap();
    let observation_id = store.next_queued_observation().await.unwrap().unwrap();
    store.project_observation(&observation_id).await.unwrap();
}

async fn scalar(db: &RegisteredGlobalDb, query: &str) -> i64 {
    let mut rows = db.read_connection().query(query, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn request(session: &str, observed: u64) -> SessionRefreshBeginOrJoinRequestV1 {
    SessionRefreshBeginOrJoinRequestV1::new(
        SessionId::new(session).unwrap(),
        SessionRefreshFrontierV1::new(observed, 0).unwrap(),
    )
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap(),
    )
}

fn zero_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

fn empty_projection_effect(recovery: &SessionRefreshRecoveryV1) -> SessionTemporalRefreshEffect {
    let next_batch = match recovery.restart_state() {
        SessionRefreshRestartStateV1::BeginProjection => 0,
        SessionRefreshRestartStateV1::ResumeProjection { next_batch_ordinal } => next_batch_ordinal,
        SessionRefreshRestartStateV1::ReadyToComplete => unreachable!(),
    };
    let committed = recovery.target_frontier().observed_through();
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        SessionRefreshFrontierV1::new(committed, committed).unwrap(),
        zero_coverage(),
        next_batch + 1,
        0,
        now(),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap()
    .with_checkpoint(next_batch, committed, committed)
    .unwrap();
    SessionTemporalRefreshEffect::Projection { progress, batch }
}

struct EmptyProjector {
    calls: std::sync::atomic::AtomicUsize,
    database: std::sync::Mutex<Option<usize>>,
}

impl EmptyProjector {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            database: std::sync::Mutex::new(None),
        }
    }
}

impl SessionTemporalRefreshProjector for EmptyProjector {
    fn project<'a>(
        &'a self,
        database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        *self.database.lock().unwrap() = Some(Arc::as_ptr(database) as usize);
        Box::pin(async move { Ok(empty_projection_effect(&recovery)) })
    }
}

#[test]
fn equivalent_wakes_coalesce_into_one_pending_pass() {
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let wake = state.handle();

    for _ in 0..32 {
        wake.wake();
    }

    assert!(state.take_dirty());
    assert!(!state.take_dirty());
}

#[test]
fn equivalent_begin_requests_join_before_the_store_boundary() {
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let wake = state.handle();
    let request = request("session.join", 4);

    assert_eq!(
        wake.request(request.clone()),
        SessionTemporalRefreshWakeDisposition::Enqueued
    );
    assert_eq!(
        wake.request(request),
        SessionTemporalRefreshWakeDisposition::Coalesced
    );
    assert_eq!(state.take_requests(8).len(), 1);
}

#[test]
fn retry_classes_use_distinct_bounded_backoff_curves() {
    let storage = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Storage, 99);
    let projector = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Projector, 99);
    let deadline = session_refresh_retry_delay(SessionTemporalRefreshRetryClass::Deadline, 99);

    assert!(storage <= std::time::Duration::from_secs(2));
    assert!(projector <= std::time::Duration::from_secs(8));
    assert!(deadline <= std::time::Duration::from_secs(4));
    assert_ne!(storage, projector);
    assert_ne!(projector, deadline);
}

#[tokio::test]
async fn admitted_effect_refreshes_to_a_real_non_empty_active_projection() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "admitted-effect", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let session_id = SessionId::new("session.refresh.real").unwrap();
    admit_canonical_effect(db, &session_id, 1, "durable refresh canary").await;

    let first = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(
        first.projected_batches, 1,
        "first refresh pass failed: {first:?}"
    );
    let second = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(second.completed, 1, "completion pass failed: {second:?}");

    let effect_count = scalar(
        db,
        "SELECT COUNT(*) FROM session_temporal_observation_effects
             WHERE session_id = 'session.refresh.real'",
    )
    .await;
    let operation_count = scalar(
        db,
        "SELECT COUNT(*) FROM session_refresh_operations
             WHERE session_id = 'session.refresh.real'",
    )
    .await;
    let occurrence_count = scalar(
        db,
        "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id = 'session.refresh.real'",
    )
    .await;
    assert_eq!(
        occurrence_count, 1,
        "effect_count={effect_count} operation_count={operation_count}"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COALESCE(SUM(occurrence_count), 0)
                 FROM session_temporal_projection_receipts
                 WHERE session_id = 'session.refresh.real'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn restart_after_materialization_resumes_from_durable_receipts() {
    let temp = TempDir::new().unwrap();
    let authority = registered_test_database(
        &temp,
        "materialization-restart",
        HostAdmissionScope::Profile,
    )
    .await;
    let db = authority.database();
    let session_id = SessionId::new("session.refresh.materialized-crash").unwrap();
    admit_canonical_effect(db, &session_id, 2, "materialized crash canary").await;
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let request = db
        .pending_session_temporal_refresh_requests_result(1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    store.begin_or_join_session_refresh(request).await.unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();

    let materialized = authority
        .project(&CanonicalSessionTemporalProjector, recovery)
        .await
        .unwrap();
    match materialized {
        SessionTemporalRefreshEffect::Projection { progress, batch } => {
            assert_eq!(batch.item_count(), 1);
            assert_eq!(progress.committed_records(), 1);
        }
        other => panic!("expected non-empty projection, got {other:?}"),
    }

    let restarted_state = Arc::new(SessionTemporalRefreshWakeState::default());
    let projected = authority
        .run_pass(
            &restarted_state,
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(projected.projected_batches, 1);
    let completed = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(completed.completed, 1);
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
                 WHERE session_id = 'session.refresh.materialized-crash'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            db,
            "SELECT COUNT(*) FROM session_occurrences
                 WHERE session_id = 'session.refresh.materialized-crash'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn new_effect_wake_is_bounded_to_its_profile_database() {
    let temp = TempDir::new().unwrap();
    let first_authority =
        registered_test_database(&temp, "first-profile", HostAdmissionScope::Profile).await;
    let second_authority =
        registered_test_database(&temp, "second-profile", HostAdmissionScope::Profile).await;
    let first_db = first_authority.database();
    let second_db = second_authority.database();
    let first_session = SessionId::new("session.refresh.profile-first").unwrap();
    let second_session = SessionId::new("session.refresh.profile-second").unwrap();
    admit_canonical_effect(first_db, &first_session, 3, "first profile canary").await;
    admit_canonical_effect(second_db, &second_session, 3, "second profile canary").await;

    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let first_wake = first_authority.ensure_profile(&registry).await;
    assert!(
        registry
            .wait_profile_idle(first_db.db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(
        scalar(
            first_db,
            "SELECT COUNT(*)
                 FROM session_occurrences AS occurrence
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                 WHERE generation.state = 'active'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(second_db, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );

    admit_canonical_effect(first_db, &first_session, 4, "second first-profile canary").await;
    first_wake.wake();
    assert!(
        registry
            .wait_profile_idle(first_db.db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(
        scalar(
            first_db,
            "SELECT COUNT(*)
                 FROM session_occurrences AS occurrence
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = occurrence.session_id
                  AND generation.generation = occurrence.generation
                 WHERE generation.state = 'active'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(second_db, "SELECT COUNT(*) FROM session_occurrences").await,
        0
    );
    registry.shutdown().await;
}

#[tokio::test]
async fn restart_finalizes_ready_progress_without_replaying_projection() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "ready-progress", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let session_id = SessionId::new("session.restart.ready").unwrap();
    let started = store
        .begin_or_join_session_refresh(request(session_id.as_str(), 0))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let coverage = TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    };
    let progress = SessionRefreshProgressV1::new(
        started.operation_id().clone(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(0, 0).unwrap(),
        coverage,
        1,
        0,
        now(),
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
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();
    let _ = store;

    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let report = authority
        .run_pass(
            &state,
            &DeferredSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(report.completed, 1, "{report:?}");
    assert_eq!(report.projected_batches, 0);

    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            started.operation_id().clone(),
            session_id.clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        receipt,
        store
            .complete_session_refresh(
                SessionRefreshCompletionRequestV1::new(
                    started.operation_id().clone(),
                    session_id,
                    progress.frontier(),
                    *progress.coverage(),
                )
                .unwrap(),
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn restart_resumes_each_committed_boundary_without_writer_fallback() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "committed-boundary", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let wake = state.handle();
    assert_eq!(
        wake.request(request("session.restart.boundaries", 0)),
        SessionTemporalRefreshWakeDisposition::Enqueued
    );
    let projector = EmptyProjector::new();

    let first = authority
        .run_pass(&state, &projector, SessionTemporalRefreshPolicy::default())
        .await;
    assert_eq!(first.begun, 1);
    assert_eq!(first.projected_batches, 1);
    assert_eq!(projector.calls.load(Ordering::Acquire), 1);
    assert_eq!(
        *projector.database.lock().unwrap(),
        Some(authority.database_identity())
    );

    let restarted_state = Arc::new(SessionTemporalRefreshWakeState::default());
    let second = authority
        .run_pass(
            &restarted_state,
            &DeferredSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(second.completed, 1, "{second:?}");
    assert_eq!(second.projected_batches, 0);
    assert!(
        crate::store::GlobalDbSessionTemporalStore::new(db)
            .running_session_refreshes()
            .await
            .unwrap()
            .is_empty()
    );
}

struct PrematureFailureProjector {
    calls: std::sync::atomic::AtomicUsize,
}

impl SessionTemporalRefreshProjector for PrematureFailureProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            Ok(SessionTemporalRefreshEffect::Fail(
                tracedecay_store::SessionRefreshFailureRequestV1::new(
                    recovery.operation_id().clone(),
                    recovery.session_id().clone(),
                    SessionRefreshFrontierV1::new(
                        recovery.target_frontier().observed_through(),
                        recovery.source_frontier(),
                    )
                    .unwrap(),
                    zero_coverage(),
                    "projector_failed",
                )
                .unwrap(),
            ))
        })
    }
}

#[tokio::test]
async fn failed_terminal_operation_is_not_retried_in_one_owner_generation() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "terminal-operation", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    store
        .begin_or_join_session_refresh(request("session.terminal.once", 0))
        .await
        .unwrap();
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let projector = PrematureFailureProjector {
        calls: std::sync::atomic::AtomicUsize::new(0),
    };

    let first = authority
        .run_pass(&state, &projector, SessionTemporalRefreshPolicy::default())
        .await;
    let second = authority
        .run_pass(&state, &projector, SessionTemporalRefreshPolicy::default())
        .await;

    assert_eq!(first.failed, 1);
    assert_eq!(first.terminal_errors, 0);
    assert_eq!(second.terminal_errors, 0);
    assert_eq!(second.failed, 0);
    assert_eq!(projector.calls.load(Ordering::Acquire), 1);
    assert!(store.running_session_refreshes().await.unwrap().is_empty());
}

struct TerminalProjector {
    cancel: bool,
}

impl SessionTemporalRefreshProjector for TerminalProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async move {
            let progress = recovery.progress().unwrap();
            if self.cancel {
                Ok(SessionTemporalRefreshEffect::Cancel(
                    tracedecay_store::SessionRefreshCancellationRequestV1::new(
                        recovery.operation_id().clone(),
                        recovery.session_id().clone(),
                        progress.frontier(),
                        *progress.coverage(),
                    ),
                ))
            } else {
                Ok(SessionTemporalRefreshEffect::Fail(
                    tracedecay_store::SessionRefreshFailureRequestV1::new(
                        recovery.operation_id().clone(),
                        recovery.session_id().clone(),
                        progress.frontier(),
                        *progress.coverage(),
                        "projector_failed",
                    )
                    .unwrap(),
                ))
            }
        })
    }
}

async fn begin_with_incomplete_progress(db: &RegisteredGlobalDb, session_id: &SessionId) {
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    store
        .begin_or_join_session_refresh(request(session_id.as_str(), 1))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(1, 0).unwrap(),
        zero_coverage(),
        1,
        0,
        now(),
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
    store
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
}

#[tokio::test]
async fn failure_and_cancel_effects_use_typed_terminal_store_operations() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "terminal-effects", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let failed_session = SessionId::new("session.effect.failed").unwrap();
    begin_with_incomplete_progress(db, &failed_session).await;
    let failed = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalProjector { cancel: false },
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(failed.failed, 1);

    let cancelled_session = SessionId::new("session.effect.cancelled").unwrap();
    begin_with_incomplete_progress(db, &cancelled_session).await;
    let cancelled = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalProjector { cancel: true },
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    assert_eq!(cancelled.cancelled, 1);

    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    assert!(
        store
            .session_refresh_recovery(&failed_session)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .session_refresh_recovery(&cancelled_session)
            .await
            .unwrap()
            .is_none()
    );
}

struct BlockingProjector {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl SessionTemporalRefreshProjector for BlockingProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        let started = Arc::clone(&self.started);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            started.notify_one();
            release.notified().await;
            Ok(empty_projection_effect(&recovery))
        })
    }
}

#[tokio::test]
async fn stale_owner_cannot_persist_after_cancellation() {
    let temp = TempDir::new().unwrap();
    let authority =
        Arc::new(registered_test_database(&temp, "stale-owner", HostAdmissionScope::Profile).await);
    let store = crate::store::GlobalDbSessionTemporalStore::new(authority.database());
    store
        .begin_or_join_session_refresh(request("session.stale.owner", 0))
        .await
        .unwrap();
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let projector = BlockingProjector {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    };
    let pass = tokio::spawn({
        let authority = Arc::clone(&authority);
        let state = Arc::clone(&state);
        async move {
            authority
                .run_pass(&state, &projector, SessionTemporalRefreshPolicy::default())
                .await
        }
    });
    started.notified().await;
    state.cancel();
    release.notify_one();
    let report = pass.await.unwrap();

    assert_eq!(report.projected_batches, 0);
    assert_eq!(
        store
            .session_refresh_recovery(&SessionId::new("session.stale.owner").unwrap())
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

struct RecordingDeferredProjector {
    sessions: std::sync::Mutex<HashSet<String>>,
}

impl RecordingDeferredProjector {
    fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashSet::new()),
        }
    }

    fn observed_session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl SessionTemporalRefreshProjector for RecordingDeferredProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(recovery.session_id().as_str().to_string());
        Box::pin(async { Ok(SessionTemporalRefreshEffect::Deferred) })
    }
}

#[tokio::test]
async fn saturated_recovery_passes_visit_every_operation_before_idling() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "saturated-recovery", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let projector = Arc::new(RecordingDeferredProjector::new());
    let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
    registry.projector = projector.clone();
    registry.policy = SessionTemporalRefreshPolicy {
        max_operations_per_pass: 2,
        ..SessionTemporalRefreshPolicy::default()
    };
    let wake = authority.ensure_profile(&registry).await;
    for index in 0..3 {
        assert_eq!(
            wake.request(request(&format!("session.saturated.{index}"), 0)),
            SessionTemporalRefreshWakeDisposition::Enqueued
        );
    }

    assert!(
        registry
            .wait_profile_idle(db.db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(projector.observed_session_count(), 3);
    registry.shutdown().await;
}

#[tokio::test]
async fn project_retirement_cancels_and_awaits_an_inflight_projector() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "project-retirement", HostAdmissionScope::Project).await;
    let owner = super::super::StoreOwnerKey {
        profile_root: temp.path().to_path_buf(),
        global_db_path: temp.path().join("global.db"),
        project_id: Some("project.retire".to_string()),
        store_root: temp.path().join("store"),
        graph_db_path: temp.path().join("store/graph.db"),
    };
    let started = Arc::new(tokio::sync::Notify::new());
    let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
    registry.projector = Arc::new(BlockingProjector {
        started: Arc::clone(&started),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let wake = authority.ensure_project(&registry, owner.clone()).await;
    assert_eq!(
        wake.request(request("session.retire.inflight", 0)),
        SessionTemporalRefreshWakeDisposition::Enqueued
    );
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_millis(250), registry.retire_project(&owner))
        .await
        .expect("retirement should cancel and await the worker promptly");

    assert_eq!(registry.project_worker_count().await, 0);
    assert_eq!(
        wake.request(request("session.retire.after", 0)),
        SessionTemporalRefreshWakeDisposition::Saturated
    );
}

struct PanicOnceProjector {
    panicked: AtomicBool,
}

impl SessionTemporalRefreshProjector for PanicOnceProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        let should_panic = !self.panicked.swap(true, Ordering::AcqRel);
        Box::pin(async move {
            assert!(!should_panic, "injected refresh worker panic");
            Ok(empty_projection_effect(&recovery))
        })
    }
}

#[tokio::test]
async fn worker_recovery_exposes_blocker_and_drains_backlog() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "worker-recovery", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let mut registry = SessionTemporalRefreshSchedulerRegistry::default();
    registry.projector = Arc::new(PanicOnceProjector {
        panicked: AtomicBool::new(false),
    });
    let wake = authority.ensure_profile(&registry).await;
    assert_eq!(
        wake.request(request("session.worker.restart", 0)),
        SessionTemporalRefreshWakeDisposition::Enqueued
    );
    let recovering = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let status = registry.profile_worker_status(db.db_path()).await;
            if status.unavailable_reason == Some(SessionTemporalRefreshUnavailableReason::Stalled) {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker should expose its stalled pre-progress recovery");
    assert_eq!(recovering.backlog, 1);
    assert_eq!(
        recovering.blocker,
        Some(SessionTemporalRefreshBlocker::WorkerPanicked)
    );
    assert_eq!(
        recovering.retry_class,
        Some(SessionTemporalRefreshRetryClass::Projector)
    );
    assert!(
        registry
            .wait_profile_idle(db.db_path(), Duration::from_secs(2))
            .await
    );
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    assert!(store.running_session_refreshes().await.unwrap().is_empty());
    let recovered = registry.profile_worker_status(db.db_path()).await;
    assert!(recovered.is_available());
    assert_eq!(recovered.backlog, 0);
    assert_eq!(recovered.blocker, None);
    assert_eq!(recovered.retry_class, None);
    assert!(recovered.last_progress_at_unix_micros.is_some());
    registry.shutdown().await;
}

struct PendingProjector;

impl SessionTemporalRefreshProjector for PendingProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(std::future::pending())
    }
}

struct TerminalErrorProjector;

impl SessionTemporalRefreshProjector for TerminalErrorProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async {
            Err(SessionTemporalRefreshProjectorError::terminal(
                "source_invalid",
            ))
        })
    }
}

#[tokio::test]
async fn terminal_projector_error_persists_a_failure_receipt() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "terminal-error", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let started = store
        .begin_or_join_session_refresh(request("session.terminal.error", 0))
        .await
        .unwrap();

    let report = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &TerminalErrorProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

    assert_eq!(report.failed, 1);
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            started.operation_id().clone(),
            started.session_id().clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Failed);
    assert_eq!(receipt.failure_code().unwrap().as_str(), "source_invalid");
}

struct NonCanonicalTerminalProjector;

impl SessionTemporalRefreshProjector for NonCanonicalTerminalProjector {
    fn project<'a>(
        &'a self,
        _database: &'a Arc<RegisteredGlobalDb>,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async {
            Err(SessionTemporalRefreshProjectorError::terminal(
                "Debug { error: \"not a failure code\" }",
            ))
        })
    }
}

#[tokio::test]
async fn noncanonical_terminal_projector_errors_persist_projector_failed() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "noncanonical-terminal", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let started = store
        .begin_or_join_session_refresh(request("session.terminal.noncanonical", 0))
        .await
        .unwrap();

    let report = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &NonCanonicalTerminalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

    assert_eq!(report.failed, 1);
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            started.operation_id().clone(),
            started.session_id().clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Failed);
    assert_eq!(receipt.failure_code().unwrap().as_str(), "projector_failed");
    assert!(store.running_session_refreshes().await.unwrap().is_empty());
}

#[tokio::test]
async fn canonical_noop_materialize_terminalizes_with_complete_receipt() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "canonical-noop", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let store = crate::store::GlobalDbSessionTemporalStore::new(db);
    let started = store
        .begin_or_join_session_refresh(request("session.canonical.noop", 0))
        .await
        .unwrap();

    let first = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;
    let second = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &CanonicalSessionTemporalProjector,
            SessionTemporalRefreshPolicy::default(),
        )
        .await;

    assert_eq!(first.projected_batches, 1);
    assert_eq!(first.deferred, 0);
    assert_eq!(second.completed, 1);
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            started.operation_id().clone(),
            started.session_id().clone(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert!(store.running_session_refreshes().await.unwrap().is_empty());
}

#[test]
fn recovery_selection_completes_by_identity_when_keys_are_skipped() {
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let mut selection = RecoverySelectionGuard::new(
        &state,
        vec![
            "session.a\0op.a".to_string(),
            "session.b\0op.b".to_string(),
            "session.c\0op.c".to_string(),
        ],
    );
    selection.complete("session.a\0op.a");
    selection.complete("session.c\0op.c");
    drop(selection);

    let pending = state
        .recovery_cycle_pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert_eq!(
        pending.iter().cloned().collect::<Vec<_>>(),
        vec!["session.b\0op.b".to_string()]
    );
}

#[tokio::test]
async fn operation_deadline_is_bounded_and_retryable_by_class() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "operation-deadline", HostAdmissionScope::Profile).await;
    crate::store::GlobalDbSessionTemporalStore::new(authority.database())
        .begin_or_join_session_refresh(request("session.deadline", 0))
        .await
        .unwrap();
    let report = authority
        .run_pass(
            &Arc::new(SessionTemporalRefreshWakeState::default()),
            &PendingProjector,
            SessionTemporalRefreshPolicy {
                operation_deadline: Duration::from_millis(10),
                ..SessionTemporalRefreshPolicy::default()
            },
        )
        .await;

    assert_eq!(report.deadline_errors, 1);
    assert_eq!(
        report.retry_class,
        Some(SessionTemporalRefreshRetryClass::Deadline)
    );
    let retryable = SessionTemporalRefreshProjectorError::retryable("source_busy");
    let terminal = SessionTemporalRefreshProjectorError::terminal("source_invalid");
    assert_eq!(
        retryable.class,
        SessionTemporalRefreshProjectorErrorClass::Retryable
    );
    assert_eq!(
        terminal.class,
        SessionTemporalRefreshProjectorErrorClass::Terminal
    );
}

#[tokio::test]
async fn profile_database_has_one_scheduler_and_equivalent_kicks_coalesce() {
    let temp = TempDir::new().unwrap();
    let authority =
        registered_test_database(&temp, "profile-coalescing", HostAdmissionScope::Profile).await;
    let db = authority.database();
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let first = authority.ensure_profile(&registry).await;
    let second = authority.ensure_profile(&registry).await;
    for _ in 0..32 {
        second.wake();
    }

    assert!(first.same_route(&second));
    assert_eq!(registry.profile_worker_count().await, 1);
    assert!(
        registry
            .wait_profile_idle(db.db_path(), Duration::from_secs(2))
            .await
    );
    assert!(registry.profile_pass_count(db.db_path()).await <= 2);
    registry.shutdown().await;
    assert_eq!(registry.profile_worker_count().await, 0);
}

#[tokio::test]
async fn project_rekey_retires_old_owner_before_rebinding_wake() {
    let temp = TempDir::new().unwrap();
    let old_authority =
        registered_test_database(&temp, "old-project", HostAdmissionScope::Project).await;
    let new_authority =
        registered_test_database(&temp, "new-project", HostAdmissionScope::Project).await;
    let old_owner = super::super::StoreOwnerKey {
        profile_root: temp.path().to_path_buf(),
        global_db_path: temp.path().join("global.db"),
        project_id: Some("project".to_string()),
        store_root: temp.path().join("old"),
        graph_db_path: temp.path().join("old/graph.db"),
    };
    let new_owner = super::super::StoreOwnerKey {
        store_root: temp.path().join("new"),
        graph_db_path: temp.path().join("new/graph.db"),
        ..old_owner.clone()
    };
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let wake = old_authority
        .ensure_project(&registry, old_owner.clone())
        .await;
    let old_state = registry.project_state(&old_owner).await.unwrap();

    new_authority
        .rekey_project(&registry, &old_owner, new_owner.clone())
        .await;
    wake.wake();

    assert!(old_state.cancelled.load(Ordering::Acquire));
    assert!(registry.project_state(&old_owner).await.is_none());
    let new_state = registry.project_state(&new_owner).await.unwrap();
    assert!(!Arc::ptr_eq(&old_state, &new_state));
    let stale = old_authority
        .ensure_project(&registry, old_owner.clone())
        .await;
    assert_eq!(
        stale.request(request("session.rekey.stale-owner", 0)),
        SessionTemporalRefreshWakeDisposition::Saturated
    );
    assert!(registry.project_state(&old_owner).await.is_none());
    assert_eq!(registry.project_worker_count().await, 1);
    assert!(new_state.take_dirty());
    registry.shutdown().await;
}
