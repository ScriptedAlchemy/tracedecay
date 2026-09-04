use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;

use super::SessionTemporalRefreshTestAuthority;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, UtcMicros,
};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::history::{
    SessionHistoricalIngestOutcome, SessionHistoricalIngestPass, SessionHistoricalIngestor,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::registry::SessionTemporalRefreshSchedulerRegistry;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use crate::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatusPort, SessionProjectionStaleReason,
};

struct ScriptedHistoricalIngestor {
    outcomes: std::sync::Mutex<VecDeque<SessionHistoricalIngestOutcome>>,
    passes: AtomicUsize,
}

impl ScriptedHistoricalIngestor {
    fn new(outcomes: impl IntoIterator<Item = SessionHistoricalIngestOutcome>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
            passes: AtomicUsize::new(0),
        }
    }
}

impl SessionHistoricalIngestor for ScriptedHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            self.passes.fetch_add(1, Ordering::AcqRel);
            self.outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or(SessionHistoricalIngestOutcome::Complete)
        })
    }

    fn cancel(&self) {}
}

struct CancelAwareHistoricalIngestor {
    entered: AtomicBool,
    exited: AtomicBool,
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}

struct RetryThenBlockHistoricalIngestor {
    passes: AtomicUsize,
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}

struct BlockThirdHistoricalIngestor {
    passes: AtomicUsize,
    third_entered: AtomicBool,
    release_third: tokio::sync::Notify,
}

struct PanicOnceHistoricalIngestor {
    passes: AtomicUsize,
}

/// Uses the real observation and projection authorities so this scheduler
/// journey catches a lost `made_progress` bit rather than testing a mock's
/// call count. The permanent Cursor failure is represented only in the
/// aggregate history result: its raw record was rejected, while Claude and
/// Codex each admitted one durable observation.
struct HealthyProvidersWithBlockedCursorIngestor {
    database: RegisteredGlobalDbLeaseV1,
    passes: AtomicUsize,
    committed: AtomicUsize,
    duplicates: AtomicUsize,
}

impl HealthyProvidersWithBlockedCursorIngestor {
    fn new(database: RegisteredGlobalDbLeaseV1) -> Self {
        Self {
            database,
            passes: AtomicUsize::new(0),
            committed: AtomicUsize::new(0),
            duplicates: AtomicUsize::new(0),
        }
    }

    async fn admit_healthy_provider(&self, provider: &str, session: &str) -> bool {
        let store = self.database.as_ref().observation_store();
        match store
            .persist_observation(healthy_provider_observation(provider, session))
            .await
            .expect("fixture observation persistence")
        {
            ObservationPersistOutcome::Committed(_) => {
                self.committed.fetch_add(1, Ordering::AcqRel);
                let observation_id = store
                    .next_queued_observation()
                    .await
                    .expect("fixture observation queue")
                    .expect("newly committed fixture observation");
                store
                    .project_observation(&observation_id)
                    .await
                    .expect("fixture observation projection");
                true
            }
            ObservationPersistOutcome::ExactDuplicate(_) => {
                self.duplicates.fetch_add(1, Ordering::AcqRel);
                false
            }
            ObservationPersistOutcome::CoveredDuplicate(_) => {
                panic!("fixture replay must be an exact duplicate")
            }
        }
    }
}

impl SessionHistoricalIngestor for HealthyProvidersWithBlockedCursorIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            self.passes.fetch_add(1, Ordering::AcqRel);
            let claude_progress = self
                .admit_healthy_provider("claude", "session.healthy.claude")
                .await;
            let codex_progress = self
                .admit_healthy_provider("codex", "session.healthy.codex")
                .await;
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "observation_cursor_advance_collision",
                made_progress: claude_progress || codex_progress,
            }
        })
    }

    fn cancel(&self) {}
}

impl SessionHistoricalIngestor for PanicOnceHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            assert!(
                self.passes.fetch_add(1, Ordering::AcqRel) != 0,
                "historical ingest panic fixture"
            );
            SessionHistoricalIngestOutcome::Complete
        })
    }

    fn cancel(&self) {}
}

impl RetryThenBlockHistoricalIngestor {
    fn new() -> Self {
        Self {
            passes: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }
}

impl BlockThirdHistoricalIngestor {
    fn new() -> Self {
        Self {
            passes: AtomicUsize::new(0),
            third_entered: AtomicBool::new(false),
            release_third: tokio::sync::Notify::new(),
        }
    }
}

impl SessionHistoricalIngestor for BlockThirdHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            match self.passes.fetch_add(1, Ordering::AcqRel) {
                0 => SessionHistoricalIngestOutcome::Complete,
                1 => SessionHistoricalIngestOutcome::Pending {
                    made_progress: false,
                },
                _ => {
                    self.third_entered.store(true, Ordering::Release);
                    self.release_third.notified().await;
                    SessionHistoricalIngestOutcome::Blocked {
                        reason_code: "fixture_complete",
                        made_progress: false,
                    }
                }
            }
        })
    }

    fn cancel(&self) {
        self.release_third.notify_waiters();
    }
}

impl SessionHistoricalIngestor for RetryThenBlockHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let pass = self.passes.fetch_add(1, Ordering::AcqRel);
            if pass == 0 {
                return SessionHistoricalIngestOutcome::Retryable {
                    reason_code: "provider_busy",
                    made_progress: false,
                };
            }
            while !self.cancelled.load(Ordering::Acquire) {
                self.wake.notified().await;
            }
            SessionHistoricalIngestOutcome::Cancelled
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

impl CancelAwareHistoricalIngestor {
    fn new() -> Self {
        Self {
            entered: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }
}

impl SessionHistoricalIngestor for CancelAwareHistoricalIngestor {
    fn run_pass(
        &self,
    ) -> Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + '_>> {
        Box::pin(async move {
            self.entered.store(true, Ordering::Release);
            while !self.cancelled.load(Ordering::Acquire) {
                self.wake.notified().await;
            }
            self.exited.store(true, Ordering::Release);
            SessionHistoricalIngestOutcome::Cancelled
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

fn healthy_provider_observation(provider: &str, session: &str) -> AnchoredObservationWrite {
    let provider = ProviderId::new(provider).expect("fixture provider");
    let session_id = SessionId::new(session).expect("fixture session");
    let source = ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
        .expect("fixture source identity");
    let generation = ObservationSourceGenerationV1::new(1).expect("fixture source generation");
    let range = ObservationSourceRangeV1::new(0, 1).expect("fixture source range");
    let record_id = ObservationId::new(format!("record.healthy.{}", provider.as_str()))
        .expect("fixture record identity");
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider.clone(),
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "healthy historical catch-up"}),
            model: Some("fixture-model".to_owned()),
            timestamp: Some(1_750_000_000),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .expect("fixture observation envelope");
    let payload = serde_json::to_value(envelope).expect("fixture observation payload");
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            generation,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .expect("fixture observation identity"),
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.healthy.{}", provider.as_str()))
                    .expect("fixture receipt identity"),
                ComponentVersion::new("sanitizer.history-scheduler-fixture.v1")
                    .expect("fixture sanitizer version"),
            )
            .expect("fixture receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).expect("fixture payload reference")),
        )
        .expect("fixture sanitization receipt"),
        RetentionClass::new("retention.history-scheduler-fixture")
            .expect("fixture retention class"),
        payload,
    )
    .expect("fixture durable observation");
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .expect("fixture next cursor");
    let write = ObservationWrite::new(observation, None, next_cursor).expect("fixture write");
    let generation = ProjectionGenerationId::new("projection.history-scheduler-fixture.v1")
        .expect("fixture projection generation");
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "history-scheduler")
            .expect("fixture resolution authorization");
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .expect("fixture retrieval anchor");
    AnchoredObservationWrite::new(write, anchor, generation).expect("fixture anchored write")
}

async fn scalar(database: &RegisteredGlobalDb, query: &str) -> i64 {
    let mut rows = database
        .read_connection()
        .query(query, ())
        .await
        .expect("fixture scalar query");
    rows.next()
        .await
        .expect("fixture scalar row read")
        .expect("fixture scalar row")
        .get(0)
        .expect("fixture scalar decode")
}

async fn profile_authority(temp: &TempDir, label: &str) -> SessionTemporalRefreshTestAuthority {
    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path().join(label))
        .await
        .unwrap();
    runtime
        .into_session_temporal_refresh_test_authority(HostAdmissionScope::Profile)
        .expect("registered profile session authority")
}

async fn wait_until(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while !predicate() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::task::yield_now().await;
    }
    true
}

#[tokio::test]
async fn retained_history_worker_wakes_again_after_idle() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-rewake").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let first = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(ingestor.passes.load(Ordering::Acquire), 1);

    let second = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(first.same_route(&second));
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) == 2,
            Duration::from_secs(2),
        )
        .await
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn history_only_retry_does_not_admit_projection_snapshots() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-only-retry").await;
    let ingestor = Arc::new(BlockThirdHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    let baseline_admissions = authority
        .database()
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.third_entered.load(Ordering::Acquire),
            Duration::from_secs(2),
        )
        .await
    );
    let after_explicit_wake = authority
        .database()
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;
    assert!(
        after_explicit_wake > baseline_admissions,
        "the explicit ensure wake must retain its projection discovery",
    );
    ingestor.release_third.notify_waiters();
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(
        authority
            .database()
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        after_explicit_wake,
        "history-only no-progress retries must not rediscover temporal projections",
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn profile_history_has_one_retained_owner() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-single-owner").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let first = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    let second = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor,
        )
        .await;

    assert!(first.same_route(&second));
    assert_eq!(registry.profile_worker_count().await, 1);
    registry.shutdown().await;
}

#[tokio::test]
async fn worker_restart_retains_historical_ingest_owner() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-worker-restart").await;
    let ingestor = Arc::new(PanicOnceHistoricalIngestor {
        passes: AtomicUsize::new(0),
    });
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;

    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) == 2,
            Duration::from_secs(2),
        )
        .await
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_and_joins_in_flight_history_pass() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-shutdown").await;
    let ingestor = Arc::new(CancelAwareHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.entered.load(Ordering::Acquire),
            Duration::from_secs(2),
        )
        .await
    );

    tokio::time::timeout(Duration::from_secs(2), registry.shutdown())
        .await
        .expect("retained history shutdown should join");
    assert!(ingestor.cancelled.load(Ordering::Acquire));
    assert!(ingestor.exited.load(Ordering::Acquire));
}

/// A saturated daemon-wide historical-ingest admission defers the pass as
/// typed retryable state — the ingestor never runs, the worker keeps cycling
/// (serving stays available) — and the deferred pass runs once a permit
/// frees, so a huge backlog on other stores cannot wedge this one.
#[tokio::test]
async fn saturated_history_admission_defers_the_pass_and_resumes() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-admission-saturated").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let admission = registry.historical_ingest_admission();
    let permits = u32::try_from(admission.available_permits()).unwrap();
    assert!(permits > 0, "the historical ingest admission is bounded");
    let held = admission
        .try_acquire_many(permits)
        .expect("occupy every historical ingest permit");

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;

    let saturated = SessionProjectionServingState::Stale {
        reason: SessionProjectionStaleReason::HistoricalRetry {
            reason_code: "history_admission_saturated".to_owned(),
        },
    };
    assert!(
        wait_until(
            || wake.serving_status().state == saturated,
            Duration::from_secs(2),
        )
        .await,
        "the deferred pass must surface as typed retryable staleness"
    );
    assert_eq!(
        ingestor.passes.load(Ordering::Acquire),
        0,
        "a saturated admission must not run the historical pass"
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await,
        "the worker keeps serving between deferred history retries"
    );
    assert_eq!(ingestor.passes.load(Ordering::Acquire), 0);

    drop(held);
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 1,
            Duration::from_secs(2),
        )
        .await,
        "the deferred pass must run once a permit frees"
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn retrying_history_is_typed_stale() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-stale-status").await;
    let ingestor = Arc::new(RetryThenBlockHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 2,
            Duration::from_secs(2),
        )
        .await
    );

    let status = wake.serving_status();
    assert_eq!(
        status.state,
        SessionProjectionServingState::Stale {
            reason: SessionProjectionStaleReason::HistoricalRetry {
                reason_code: "provider_busy".to_owned(),
            },
        }
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn blocked_cursor_failure_projects_healthy_provider_progress_once_across_restart() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-blocked-progress").await;
    let ingestor = Arc::new(HealthyProvidersWithBlockedCursorIngestor::new(
        authority.database.clone(),
    ));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 1,
            Duration::from_secs(2),
        )
        .await,
        "the initial historical sweep must run"
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await,
        "healthy provider progress must request temporal projection even while Cursor is blocked"
    );
    assert_eq!(
        wake.serving_status().state,
        SessionProjectionServingState::Stale {
            reason: SessionProjectionStaleReason::HistoricalBlocked {
                reason_code: "observation_cursor_advance_collision".to_owned(),
            },
        },
        "a permanent Cursor failure remains typed blocked"
    );
    assert!(
        wake.serving_status().last_progress_at_unix_micros.is_some(),
        "the healthy Claude and Codex rows must be recorded as progress"
    );
    assert_eq!(
        scalar(
            authority.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id IN ('session.healthy.claude', 'session.healthy.codex')",
        )
        .await,
        2,
        "both healthy providers must advance the temporal projection"
    );
    registry.shutdown().await;

    let restarted = SessionTemporalRefreshSchedulerRegistry::default();
    let restarted_wake = restarted
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 2,
            Duration::from_secs(2),
        )
        .await,
        "the restarted worker must replay the retained historical source"
    );
    assert!(
        restarted
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(
        restarted_wake.serving_status().state,
        SessionProjectionServingState::Stale {
            reason: SessionProjectionStaleReason::HistoricalBlocked {
                reason_code: "observation_cursor_advance_collision".to_owned(),
            },
        },
        "restart preserves the typed blocked Cursor state"
    );
    assert_eq!(
        scalar(
            authority.database(),
            "SELECT COUNT(*) FROM session_occurrences
             WHERE session_id IN ('session.healthy.claude', 'session.healthy.codex')",
        )
        .await,
        2,
        "replayed Claude and Codex inputs must not duplicate temporal occurrences"
    );
    assert_eq!(ingestor.committed.load(Ordering::Acquire), 2);
    assert_eq!(ingestor.duplicates.load(Ordering::Acquire), 2);

    restarted.shutdown().await;
}
