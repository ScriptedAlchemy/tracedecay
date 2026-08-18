//! Retained recovery from durable Work receipts into the observability outbox.

use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tracedecay_application::{
    ApplicationContractError, PendingWorkOwnerObservationV1, WorkOwnerObservationReceiptV1,
    WorkOwnerObservationScanCursorV1, WorkOwnerObservationStoragePortV1,
};
use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;

use super::BoundedObservabilityProducerV1;
use super::work_duplicate_emit::work_duplicate_observation_envelope;
use super::work_retry_leak_emit::{
    work_leak_observation_envelope, work_retry_observation_envelope,
};

const RECOVERY_RUNNING: u8 = 0;
const RECOVERY_STOPPING: u8 = 1;
const RECOVERY_STOPPED: u8 = 2;
const RECOVERY_BATCH: u16 = 256;
const RECOVERY_INTERVAL: Duration = Duration::from_secs(1);
const RECOVERY_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkOwnerObservationRecoverySummaryV1 {
    pub marked_durable: u64,
    pub failed: u64,
}

enum RecoveryControl {
    Shutdown {
        reply: oneshot::Sender<WorkOwnerObservationRecoverySummaryV1>,
    },
}

/// Project-owned bounded recovery worker. It quiesces before its producer so
/// no durable source marker can be advanced after observability shutdown.
pub struct WorkOwnerObservationRecoveryV1 {
    control: mpsc::Sender<RecoveryControl>,
    state: Arc<AtomicU8>,
    admission: Mutex<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl WorkOwnerObservationRecoveryV1 {
    pub fn start(
        storage: WorkSqliteStorage,
        producer: Arc<BoundedObservabilityProducerV1>,
    ) -> Result<Self, &'static str> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "work_owner_observation_recovery_runtime_unavailable")?;
        let (control, control_rx) = mpsc::channel(1);
        let state = Arc::new(AtomicU8::new(RECOVERY_RUNNING));
        let worker_state = Arc::clone(&state);
        let worker = runtime.spawn(run_recovery(storage, producer, control_rx, worker_state));
        Ok(Self {
            control,
            state,
            admission: Mutex::new(()),
            worker: Mutex::new(Some(worker)),
        })
    }

    pub async fn shutdown(
        &self,
    ) -> Result<WorkOwnerObservationRecoverySummaryV1, ApplicationContractError> {
        {
            let _admission = self.admission.lock().map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery lock poisoned".to_owned(),
                )
            })?;
            self.state
                .compare_exchange(
                    RECOVERY_RUNNING,
                    RECOVERY_STOPPING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map_err(|_| {
                    ApplicationContractError::Domain(
                        "work owner-observation recovery closed".to_owned(),
                    )
                })?;
        }
        let (reply, result) = oneshot::channel();
        self.control
            .try_send(RecoveryControl::Shutdown { reply })
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery control closed".to_owned(),
                )
            })?;
        let worker = self
            .worker
            .lock()
            .map_err(|_| {
                ApplicationContractError::Domain(
                    "work owner-observation recovery lock poisoned".to_owned(),
                )
            })?
            .take();
        let summary = match timeout(RECOVERY_SHUTDOWN_DEADLINE, result).await {
            Ok(Ok(summary)) => summary,
            Ok(Err(_)) => {
                return Err(ApplicationContractError::Domain(
                    "work owner-observation recovery stopped".to_owned(),
                ));
            }
            Err(_) => {
                if let Some(worker) = worker {
                    worker.abort();
                    let _ = worker.await;
                }
                self.state.store(RECOVERY_STOPPED, Ordering::Release);
                return Err(ApplicationContractError::Domain(
                    "work owner-observation recovery shutdown deadline".to_owned(),
                ));
            }
        };
        if let Some(worker) = worker {
            worker.await.map_err(|error| {
                ApplicationContractError::Domain(format!(
                    "work owner-observation recovery join failed: {error}"
                ))
            })?;
        }
        Ok(summary)
    }
}

async fn run_recovery<S>(
    storage: S,
    producer: Arc<BoundedObservabilityProducerV1>,
    mut control: mpsc::Receiver<RecoveryControl>,
    state: Arc<AtomicU8>,
) where
    S: WorkOwnerObservationStoragePortV1 + Clone + Send + Sync + 'static,
{
    let mut schedule = interval(RECOVERY_INTERVAL);
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut summary = WorkOwnerObservationRecoverySummaryV1::default();
    let mut cursor = None;
    loop {
        tokio::select! {
            _ = schedule.tick() => recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await,
            Some(RecoveryControl::Shutdown { reply }) = control.recv() => {
                recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await;
                state.store(RECOVERY_STOPPED, Ordering::Release);
                let _ = reply.send(summary);
                return;
            }
            else => {
                state.store(RECOVERY_STOPPED, Ordering::Release);
                return;
            }
        }
    }
}

async fn recover_batch<S>(
    storage: &S,
    producer: &BoundedObservabilityProducerV1,
    cursor: &mut Option<WorkOwnerObservationScanCursorV1>,
    summary: &mut WorkOwnerObservationRecoverySummaryV1,
) where
    S: WorkOwnerObservationStoragePortV1 + Clone + Send + Sync + 'static,
{
    let Some(limit) = NonZeroU16::new(RECOVERY_BATCH) else {
        summary.failed = summary.failed.saturating_add(1);
        return;
    };
    let scan_storage = S::clone(storage);
    let after = cursor.clone();
    let pending = match tokio::task::spawn_blocking(move || {
        scan_storage.pending_owner_observations(after.as_ref(), limit)
    })
    .await
    {
        Ok(Ok(pending)) => pending,
        Ok(Err(error)) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation pending scan failed");
            return;
        }
        Err(error) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation pending scan task failed");
            return;
        }
    };
    let wrapped = pending.len() < usize::from(RECOVERY_BATCH);
    for pending in pending {
        *cursor = Some(pending.scan_cursor.clone());
        recover_one(storage, producer, pending, summary).await;
    }
    if wrapped {
        *cursor = None;
    }
}

async fn recover_one<S>(
    storage: &S,
    producer: &BoundedObservabilityProducerV1,
    pending: PendingWorkOwnerObservationV1,
    summary: &mut WorkOwnerObservationRecoverySummaryV1,
) where
    S: WorkOwnerObservationStoragePortV1 + Clone + Send + Sync + 'static,
{
    if !pending.validate() {
        summary.failed = summary.failed.saturating_add(1);
        return;
    }
    let scope = pending.marker.authority.project_id().as_str();
    let envelope = match &pending.receipt {
        WorkOwnerObservationReceiptV1::Retry(receipt) => {
            work_retry_observation_envelope(producer.identity(), scope, receipt)
        }
        WorkOwnerObservationReceiptV1::Leak(receipt) => {
            work_leak_observation_envelope(producer.identity(), scope, receipt)
        }
        WorkOwnerObservationReceiptV1::Duplicate(receipt) => work_duplicate_observation_envelope(
            producer.identity(),
            scope,
            &pending.marker.authority,
            receipt,
        ),
    };
    let Some(envelope) = envelope else {
        summary.failed = summary.failed.saturating_add(1);
        return;
    };
    if let Err(error) = producer.emit_owner_fact(envelope).await {
        summary.failed = summary.failed.saturating_add(1);
        tracing::warn!(%error, "Work owner-observation durable claim failed");
        return;
    }
    let marker_storage = S::clone(storage);
    let marker = pending.marker;
    match tokio::task::spawn_blocking(move || {
        marker_storage.mark_owner_observation_durable(&marker)
    })
    .await
    {
        Ok(Ok(_)) => summary.marked_durable = summary.marked_durable.saturating_add(1),
        Ok(Err(error)) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation source marker CAS failed");
        }
        Err(error) => {
            summary.failed = summary.failed.saturating_add(1);
            tracing::warn!(%error, "Work owner-observation source marker task failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU16;
    use tracedecay_application::{
        PendingWorkOwnerObservationV1, WorkOwnerObservationMarkOutcomeV1,
        WorkOwnerObservationMarkerV1, WorkOwnerObservationStorageErrorV1,
    };
    use tracedecay_domain::{
        ActorId, AttemptId, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
        ManifestDigest, ProjectId, ProjectionGenerationId, QuantityEvidenceClassV1, RepositoryId,
        RunId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority, WorkCommandId,
        WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationEvidenceV1,
        WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationReceiptV1,
        WorkDuplicateAdjudicationRevisionV1, WorkTopologyGenerationRefV1, WorktreeId,
        canonical_sha256,
    };
    use tracedecay_runtime_core::db::engine::params;

    const PROJECT_ID: &str = "project.owner-observation-recovery";

    fn id<T>(value: impl Into<String>) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.into()).unwrap()
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn attempt(ordinal: u32, suffix: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            id::<TaskId>(format!("task.recovery.{ordinal}.{suffix}")),
            id::<RunId>(format!("run.recovery.{ordinal}.{suffix}")),
            id::<AttemptId>(format!("attempt.recovery.{ordinal}.{suffix}")),
        )
        .unwrap()
    }

    async fn insert_pending_duplicate(db: &tracedecay_global_db::RegisteredGlobalDb, ordinal: u32) {
        let authority = WorkAuthority::new(
            id::<ProjectId>(PROJECT_ID),
            id::<RepositoryId>("repository.owner-observation-recovery"),
            id::<WorktreeId>("worktree.owner-observation-recovery"),
            id::<ActorId>("actor.owner-observation-recovery"),
            digest('a'),
        )
        .unwrap();
        let command = WorkDuplicateAdjudicationCommandV1 {
            expected_revision: None,
            first_attempt: attempt(ordinal, "first"),
            second_attempt: attempt(ordinal, "second"),
            evidence: WorkDuplicateAdjudicationEvidenceV1 {
                work_generation: id::<ProjectionGenerationId>(format!(
                    "generation.recovery.{ordinal}"
                )),
                topology_generation: WorkTopologyGenerationRefV1::new(format!(
                    "sha256:{ordinal:064x}"
                ))
                .unwrap(),
            },
            verdict: DuplicateEffortKindV1::ExactDuplicate,
            quantities: WorkDuplicateAdjudicationQuantitiesV1 {
                wall_micros: Some(10),
                token_count: None,
                cost_micros: None,
                test_count: None,
                effect_count: None,
                evidence: QuantityEvidenceClassV1::OwnerReceipt,
                effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
                coverage: CoverageStateV1::Known,
            },
            reason: "direct recovery worker test".to_owned(),
            command_id: id::<WorkCommandId>(format!("command.recovery.{ordinal:04}")),
            occurred_at: UtcMicros(i64::from(ordinal) + 10),
        }
        .canonicalized();
        let canonical_input_digest = command.canonical_input_digest().unwrap();
        let receipt = WorkDuplicateAdjudicationReceiptV1::new(
            &authority,
            command,
            WorkDuplicateAdjudicationRevisionV1::initial(),
            canonical_input_digest.clone(),
        )
        .unwrap();
        let receipt_digest =
            canonical_sha256(&WorkOwnerObservationReceiptV1::Duplicate(receipt.clone())).unwrap();
        db.writer_connection()
            .unwrap()
            .execute(
                "INSERT INTO work_duplicate_adjudications_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    relation_digest, revision, command_id, canonical_input_digest,
                    work_generation, topology_generation, occurred_at, receipt_digest,
                    observation_state, receipt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                           'pending', ?14)",
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    receipt.adjudication_ref().as_str(),
                    1_i64,
                    receipt.command().command_id.as_str(),
                    canonical_input_digest.as_str(),
                    receipt.command().evidence.work_generation.as_str(),
                    receipt.command().evidence.topology_generation.as_str(),
                    receipt.command().occurred_at.0,
                    receipt_digest.as_str(),
                    serde_json::to_string(&receipt).unwrap(),
                ],
            )
            .await
            .unwrap();
    }

    fn producer(
        db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    ) -> Arc<BoundedObservabilityProducerV1> {
        Arc::new(
            BoundedObservabilityProducerV1::start(
                db,
                super::super::ObservabilityProducerIdentityV1 {
                    authorized_scope_ref: PROJECT_ID.to_owned(),
                    process_boot_id: "boot.owner-observation-recovery".to_owned(),
                    producer_revision: "owner-observation-recovery-test.v1".to_owned(),
                    configuration_revision: "configuration.owner-observation-recovery".to_owned(),
                    policy_revision: "policy.owner-observation-recovery".to_owned(),
                },
                1_024,
            )
            .unwrap(),
        )
    }

    #[derive(Clone)]
    struct ConflictMarkerStorage {
        source: WorkSqliteStorage,
    }

    impl WorkOwnerObservationStoragePortV1 for ConflictMarkerStorage {
        fn pending_owner_observations(
            &self,
            after: Option<&WorkOwnerObservationScanCursorV1>,
            limit: NonZeroU16,
        ) -> Result<Vec<PendingWorkOwnerObservationV1>, WorkOwnerObservationStorageErrorV1>
        {
            self.source.pending_owner_observations(after, limit)
        }

        fn mark_owner_observation_durable(
            &self,
            _marker: &WorkOwnerObservationMarkerV1,
        ) -> Result<WorkOwnerObservationMarkOutcomeV1, WorkOwnerObservationStorageErrorV1> {
            Err(WorkOwnerObservationStorageErrorV1::Conflict)
        }
    }

    #[tokio::test]
    async fn preexisting_receipt_recovers_before_shutdown_and_worker_quiesces() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "owner-observation-restart",
        )
        .await;
        insert_pending_duplicate(&harness.registered, 1).await;
        let storage = harness.registered.work_storage().unwrap();
        let producer = producer(harness.registered.clone());
        let recovery =
            WorkOwnerObservationRecoveryV1::start(storage.clone(), Arc::clone(&producer)).unwrap();

        let summary = recovery.shutdown().await.unwrap();
        assert_eq!(summary.marked_durable, 1);
        assert_eq!(summary.failed, 0);
        assert!(
            storage
                .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
                .unwrap()
                .is_empty()
        );

        insert_pending_duplicate(&harness.registered, 2).await;
        tokio::task::yield_now().await;
        assert_eq!(
            storage
                .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
                .unwrap()
                .len(),
            1,
            "shutdown must stop future recovery scans"
        );
        producer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_marker_cas_retries_exact_durable_owner_fact() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "owner-observation-cas-retry",
        )
        .await;
        insert_pending_duplicate(&harness.registered, 3).await;
        let storage = harness.registered.work_storage().unwrap();
        let pending = storage
            .pending_owner_observations(None, NonZeroU16::new(1).unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let producer = producer(harness.registered.clone());
        let mut summary = WorkOwnerObservationRecoverySummaryV1::default();

        recover_one(
            &ConflictMarkerStorage {
                source: storage.clone(),
            },
            producer.as_ref(),
            pending.clone(),
            &mut summary,
        )
        .await;
        assert_eq!(summary.marked_durable, 0);
        assert_eq!(summary.failed, 1);
        assert_eq!(
            storage
                .pending_owner_observations(None, NonZeroU16::new(1).unwrap())
                .unwrap()
                .len(),
            1
        );

        recover_one(&storage, producer.as_ref(), pending, &mut summary).await;
        assert_eq!(summary.marked_durable, 1);
        assert_eq!(summary.failed, 1);
        assert!(
            storage
                .pending_owner_observations(None, NonZeroU16::new(1).unwrap())
                .unwrap()
                .is_empty()
        );
        producer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pagination_crosses_recovery_batch_without_skipping_receipts() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "owner-observation-pagination",
        )
        .await;
        for ordinal in 1..=257 {
            insert_pending_duplicate(&harness.registered, ordinal).await;
        }
        let storage = harness.registered.work_storage().unwrap();
        let producer = producer(harness.registered.clone());
        let mut summary = WorkOwnerObservationRecoverySummaryV1::default();
        let mut cursor = None;

        recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await;
        assert_eq!(summary.marked_durable, 256);
        assert!(cursor.is_some());
        recover_batch(&storage, producer.as_ref(), &mut cursor, &mut summary).await;
        assert_eq!(summary.marked_durable, 257);
        assert_eq!(summary.failed, 0);
        assert!(cursor.is_none());
        assert!(
            storage
                .pending_owner_observations(None, NonZeroU16::new(1).unwrap())
                .unwrap()
                .is_empty()
        );
        producer.shutdown().await.unwrap();
    }
}
