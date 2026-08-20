use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, Savepoint, Transaction};
use tracedecay_domain::{
    FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, PayloadAccessState, ProjectId, ProvenanceId, UtcMicros,
};
use tracedecay_store::{
    AdmissionConfigV1, AnchorDispositionReasonClassV1, AnchorDispositionStateV1, CommitSequenceV1,
    FactWriteBatch, IdempotencyIdentityV1, LocatorDigest, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RetrievalAnchorDispositionRecordV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, StorageRuntimeErrorV1, StoreCommitReceiptV1, StoreRuntimeBindingV1,
    VerifiedStoreLocatorV1,
};

use super::*;
use crate::{
    checkpoint::{
        CheckpointBlockers, CheckpointDecision, CheckpointInterruption, CheckpointKind,
        CheckpointMode, CheckpointOutcome, CheckpointPressure, CheckpointReport, CheckpointResult,
        MaintenanceCheckpointMode, WalPressure, WalSample,
    },
    maintenance::{ExclusiveMaintenancePermit, MaintenanceOwnerId},
    test_support::{binding, metadata, request, scope},
};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        let nonce = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tracedecay-writer-{}-{now}-{nonce}.db",
            std::process::id()
        ));
        std::fs::File::create(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-journal", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
    }
}

struct TestPersistence {
    applied: Arc<AtomicU64>,
    sequence: u64,
}

struct BlockingPersistence {
    entered: mpsc::Sender<u64>,
    release: mpsc::Receiver<()>,
    sequence: u64,
}

struct ToggleAuthority {
    allowed: Arc<AtomicBool>,
}

impl RuntimeWriteAuthority for ToggleAuthority {
    fn verify(&self, _stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        if self.allowed.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(RuntimeWriteAuthorityError::denied(
                "test runtime write authority revoked",
            ))
        }
    }
}

struct RevokeAfterAdmissionAuthority {
    admitted: AtomicBool,
}

impl RuntimeWriteAuthority for RevokeAfterAdmissionAuthority {
    fn verify(&self, stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        if stage == RuntimeWriteAuthorityStage::BeforeAdmission
            && !self.admitted.swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }
        Err(RuntimeWriteAuthorityError::denied(
            "test runtime write authority revoked after admission",
        ))
    }
}

struct RecordingCheckpointAuthority {
    stages: Arc<Mutex<Vec<RuntimeWriteAuthorityStage>>>,
    denied_stage: Option<RuntimeWriteAuthorityStage>,
}

struct DenyThirdBeforeCommitAuthority {
    before_commit_checks: AtomicU64,
}

impl RuntimeWriteAuthority for DenyThirdBeforeCommitAuthority {
    fn verify(&self, stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        if stage == RuntimeWriteAuthorityStage::BeforeCommit
            && self.before_commit_checks.fetch_add(1, Ordering::SeqCst) >= 2
        {
            Err(RuntimeWriteAuthorityError::denied(
                "test backup authority denied before publication",
            ))
        } else {
            Ok(())
        }
    }
}

impl RuntimeWriteAuthority for RecordingCheckpointAuthority {
    fn verify(&self, stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        self.stages.lock().unwrap().push(stage);
        if self.denied_stage == Some(stage) {
            Err(RuntimeWriteAuthorityError::denied(
                "test checkpoint authority denied",
            ))
        } else {
            Ok(())
        }
    }
}

struct RevokingPersistence {
    inner: TestPersistence,
    allowed: Arc<AtomicBool>,
}

struct CancellingFirstRequestPersistence {
    first_probe: Arc<Probe>,
    sequence: u64,
}

struct LongRunningPersistence;

impl WriterPersistence for LongRunningPersistence {
    fn lookup_idempotency(
        &mut self,
        _transaction: &Transaction<'_>,
        _binding: &StoreRuntimeBindingV1,
        _idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        Ok(None)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        _binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        savepoint
            .query_row(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT sum(x) FROM n",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|_| settlement::infrastructure("run long cancellation query"))?;
        let metadata = &request.envelope().metadata;
        Ok(StoreCommitReceiptV1 {
            operation_id: metadata.operation_id.clone(),
            idempotency: metadata.idempotency.clone(),
            shard_id: metadata.shard_id.clone(),
            incarnation: metadata.incarnation,
            authority_epoch: metadata.authority_epoch,
            commit_sequence: CommitSequenceV1(1),
            committed_at: metadata.admitted_at,
        })
    }
}

impl WriterPersistence for CancellingFirstRequestPersistence {
    fn lookup_idempotency(
        &mut self,
        _transaction: &Transaction<'_>,
        _binding: &StoreRuntimeBindingV1,
        _idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        Ok(None)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        _binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        savepoint
            .execute_batch("CREATE TABLE IF NOT EXISTS cancellation_batch (value INTEGER NOT NULL)")
            .map_err(|_| settlement::infrastructure("create cancellation batch table"))?;
        self.sequence += 1;
        if self.sequence == 1 {
            self.first_probe.interruption.store(1, Ordering::SeqCst);
        } else {
            savepoint
                .query_row(
                    "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT sum(x) FROM n",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|_| settlement::infrastructure("run unrelated batch query"))?;
        }
        let sequence = i64::try_from(self.sequence)
            .map_err(|_| settlement::infrastructure("convert cancellation batch marker"))?;
        savepoint
            .execute(
                "INSERT INTO cancellation_batch(value) VALUES (?1)",
                [sequence],
            )
            .map_err(|_| settlement::infrastructure("insert cancellation batch marker"))?;
        let metadata = &request.envelope().metadata;
        Ok(StoreCommitReceiptV1 {
            operation_id: metadata.operation_id.clone(),
            idempotency: metadata.idempotency.clone(),
            shard_id: metadata.shard_id.clone(),
            incarnation: metadata.incarnation,
            authority_epoch: metadata.authority_epoch,
            commit_sequence: CommitSequenceV1(self.sequence),
            committed_at: metadata.admitted_at,
        })
    }
}

impl WriterPersistence for RevokingPersistence {
    fn lookup_idempotency(
        &mut self,
        transaction: &Transaction<'_>,
        binding: &StoreRuntimeBindingV1,
        idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        self.inner
            .lookup_idempotency(transaction, binding, idempotency)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        let receipt = self.inner.apply_and_record(savepoint, binding, request)?;
        self.allowed.store(false, Ordering::SeqCst);
        Ok(receipt)
    }
}

impl WriterPersistence for TestPersistence {
    fn lookup_idempotency(
        &mut self,
        _transaction: &Transaction<'_>,
        _binding: &StoreRuntimeBindingV1,
        _idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        Ok(None)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        _binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        savepoint
            .execute_batch("CREATE TABLE IF NOT EXISTS writer_test (value INTEGER NOT NULL)")
            .map_err(|_| settlement::infrastructure("create test table"))?;
        savepoint
            .execute("INSERT INTO writer_test(value) VALUES (1)", [])
            .map_err(|_| settlement::infrastructure("insert test marker"))?;
        self.applied.fetch_add(1, Ordering::SeqCst);
        self.sequence += 1;
        let metadata = &request.envelope().metadata;
        Ok(StoreCommitReceiptV1 {
            operation_id: metadata.operation_id.clone(),
            idempotency: metadata.idempotency.clone(),
            shard_id: metadata.shard_id.clone(),
            incarnation: metadata.incarnation,
            authority_epoch: metadata.authority_epoch,
            commit_sequence: CommitSequenceV1(self.sequence),
            committed_at: metadata.admitted_at,
        })
    }
}

impl WriterPersistence for BlockingPersistence {
    fn lookup_idempotency(
        &mut self,
        _transaction: &Transaction<'_>,
        _binding: &StoreRuntimeBindingV1,
        _idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        Ok(None)
    }

    fn apply_and_record(
        &mut self,
        savepoint: &mut Savepoint<'_>,
        _binding: &StoreRuntimeBindingV1,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        self.sequence += 1;
        self.entered
            .send(self.sequence)
            .map_err(|_| settlement::infrastructure("report blocked test write"))?;
        self.release
            .recv()
            .map_err(|_| settlement::infrastructure("release blocked test write"))?;
        savepoint
            .execute_batch("CREATE TABLE IF NOT EXISTS maintenance_order (value INTEGER NOT NULL)")
            .map_err(|_| settlement::infrastructure("create maintenance order table"))?;
        let sequence = i64::try_from(self.sequence)
            .map_err(|_| settlement::infrastructure("convert maintenance order marker"))?;
        savepoint
            .execute(
                "INSERT INTO maintenance_order(value) VALUES (?1)",
                [sequence],
            )
            .map_err(|_| settlement::infrastructure("insert maintenance order marker"))?;
        let metadata = &request.envelope().metadata;
        Ok(StoreCommitReceiptV1 {
            operation_id: metadata.operation_id.clone(),
            idempotency: metadata.idempotency.clone(),
            shard_id: metadata.shard_id.clone(),
            incarnation: metadata.incarnation,
            authority_epoch: metadata.authority_epoch,
            commit_sequence: CommitSequenceV1(self.sequence),
            committed_at: metadata.admitted_at,
        })
    }
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: AtomicU8,
    commit_started: AtomicBool,
}

struct DelayedInterruptionProbe {
    inner: Probe,
    checks_before_interruption: AtomicU64,
    interruption: RuntimeInterruptionV1,
}

impl RuntimeRequestProbeV1 for DelayedInterruptionProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        self.inner.cancellation_identity()
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        self.inner.deadline_identity()
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if self
            .checks_before_interruption
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            None
        } else {
            Some(self.interruption)
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none() && self.inner.try_begin_commit()
    }
}

impl Probe {
    fn new(request: &RuntimeSubmitRequestV1, interruption: Option<RuntimeInterruptionV1>) -> Self {
        Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption: AtomicU8::new(match interruption {
                None => 0,
                Some(RuntimeInterruptionV1::Cancelled) => 1,
                Some(RuntimeInterruptionV1::DeadlineExceeded) => 2,
            }),
            commit_started: AtomicBool::new(false),
        }
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }
    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }
    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        match self.interruption.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(RuntimeInterruptionV1::Cancelled),
            2 => Some(RuntimeInterruptionV1::DeadlineExceeded),
            _ => unreachable!(),
        }
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

fn start(
    database: &TestDatabase,
    request: &RuntimeSubmitRequestV1,
    applied: Arc<AtomicU64>,
) -> PersistentWriter {
    let binding = binding(&request.envelope().metadata);
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
    );
    PersistentWriter::start_with_persistence(
        ExistingWriterLocator::new(binding, locator, database.0.clone()).unwrap(),
        AdmissionConfigV1::default(),
        Box::new(TestPersistence {
            applied,
            sequence: 0,
        }),
    )
    .unwrap()
}

fn start_with_persistence(
    database: &TestDatabase,
    request: &RuntimeSubmitRequestV1,
    persistence: Box<dyn WriterPersistence>,
) -> PersistentWriter {
    let binding = binding(&request.envelope().metadata);
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
    );
    PersistentWriter::start_with_persistence(
        ExistingWriterLocator::new(binding, locator, database.0.clone()).unwrap(),
        AdmissionConfigV1::default(),
        persistence,
    )
    .unwrap()
}

fn fact_request(operation: &str, key: &str, digest_byte: char) -> RuntimeSubmitRequestV1 {
    let metadata = metadata(operation, key, digest_byte);
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.runtime").unwrap(),
    };
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new(operation).unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(1),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(fact_id, owner, None, vec![event], vec![], vec![], None)
        .unwrap()
        .with_identity_material(identity)
        .unwrap();
    let transaction_scope = scope(&metadata);
    let control = request(metadata.clone()).control().clone();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::Fact(Box::new(batch)),
        },
        transaction_scope,
        control,
    )
    .unwrap()
}

fn project_fixture_request(
    operation: &str,
    key: &str,
    digest_byte: char,
    payload: RepositoryWritePayloadV1,
) -> RuntimeSubmitRequestV1 {
    let mut metadata_value = serde_json::to_value(metadata(operation, key, digest_byte)).unwrap();
    metadata_value["shard_id"]["profile_id"] = serde_json::json!("profile.fixture");
    metadata_value["shard_id"]["scope"]["project_id"] = serde_json::json!("project.fixture");
    let metadata = serde_json::from_value(metadata_value).unwrap();
    let transaction_scope = scope(&metadata);
    let control = request(metadata.clone()).control().clone();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .unwrap()
}

#[test]
fn actor_commits_before_reply_and_releases_admission() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.writer", "key.writer", 'a'));
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &request, Arc::clone(&applied));
    let checkpoint = writer.checkpoint_handle();
    assert_eq!(checkpoint.binding(), writer.binding());
    let mut checkpoint_status = checkpoint.status_subscription();
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let outcome = runtime.block_on(writer.submit(request, probe)).unwrap();
    assert!(matches!(outcome, RuntimeSubmitOutcomeV1::Committed { .. }));
    runtime
        .block_on(checkpoint_status.changed())
        .expect("writer publishes a scheduled WAL sample");
    assert!(matches!(
        checkpoint_status.borrow().latest.as_ref(),
        Some(CheckpointOutcome::BelowSoft { .. })
    ));
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    assert_eq!(writer.telemetry_snapshot().queue.queued_operations, 0);
    let rows: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writer_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn competing_write_authority_fails_instead_of_reporting_retryable_saturation() {
    let database = TestDatabase::new();
    let blocked = request(metadata(
        "operation.writer.competing",
        "key.writer.competing",
        'b',
    ));
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &blocked, Arc::clone(&applied));
    let mut competing = Connection::open(&database.0).unwrap();
    let transaction = competing
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let failure = runtime
        .block_on(writer.submit(blocked.clone(), Arc::new(Probe::new(&blocked, None))))
        .unwrap_err();
    assert!(matches!(
        failure,
        WriterActorError::StorageFailure(StorageRuntimeErrorV1::Infrastructure { operation })
            if operation.contains("competing write authority")
    ));
    assert_eq!(applied.load(Ordering::SeqCst), 0);

    drop(transaction);
    let recovered = request(metadata(
        "operation.writer.recovered",
        "key.writer.recovered",
        'c',
    ));
    assert!(matches!(
        runtime
            .block_on(writer.submit(recovered.clone(), Arc::new(Probe::new(&recovered, None)),))
            .unwrap(),
        RuntimeSubmitOutcomeV1::Committed { .. }
    ));
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    writer.shutdown_and_join().unwrap();
}

mod authority;
mod backup;
mod checkpoint;
mod interruption;
