use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
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

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: AtomicU8,
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
    let batch = FactWriteBatch::new(
        fact_id,
        owner,
        None,
        vec![event],
        vec![],
        vec![],
        None,
        None,
    )
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
fn checkpoint_control_surfaces_typed_deadline_and_admission_signal() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.checkpoint", "key.checkpoint", 'p'));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
    let probe = Arc::new(Probe::new(
        &request,
        Some(RuntimeInterruptionV1::DeadlineExceeded),
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let result = runtime
        .block_on(async {
            checkpoint
                .trigger(CheckpointRequest::new(CheckpointBlockers::default(), probe))
                .unwrap()
                .wait()
                .await
        })
        .unwrap();

    assert!(matches!(
        result,
        CheckpointOutcome::Interrupted {
            reason: CheckpointInterruption::DeadlineExceeded,
            wal: None,
            ..
        }
    ));
    assert_eq!(checkpoint.pressure(), CheckpointPressure::Open);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn checkpoint_rechecks_the_same_authority_before_publication() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.checkpoint.authority",
        "key.checkpoint.authority",
        'a',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let stages = Arc::new(Mutex::new(Vec::new()));
    let authority = Arc::new(RecordingCheckpointAuthority {
        stages: Arc::clone(&stages),
        denied_stage: None,
    });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    runtime
        .block_on(
            checkpoint
                .trigger_authorized(
                    CheckpointRequest::new(CheckpointBlockers::default(), probe),
                    authority,
                )
                .unwrap()
                .wait(),
        )
        .unwrap();

    assert_eq!(
        *stages.lock().unwrap(),
        [
            RuntimeWriteAuthorityStage::BeforeAdmission,
            RuntimeWriteAuthorityStage::Dequeued,
            RuntimeWriteAuthorityStage::BeforeCommit,
        ]
    );
    assert!(checkpoint.status().latest.is_some());
    writer.shutdown_and_join().unwrap();
}

#[test]
fn checkpoint_authority_loss_is_typed_and_never_published() {
    for denied_stage in [
        RuntimeWriteAuthorityStage::BeforeAdmission,
        RuntimeWriteAuthorityStage::Dequeued,
        RuntimeWriteAuthorityStage::BeforeCommit,
    ] {
        let database = TestDatabase::new();
        let request = request(metadata(
            "operation.checkpoint.revoked",
            "key.checkpoint.revoked",
            'r',
        ));
        let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
        let checkpoint = writer.checkpoint_handle();
        let authority = Arc::new(RecordingCheckpointAuthority {
            stages: Arc::new(Mutex::new(Vec::new())),
            denied_stage: Some(denied_stage),
        });
        let probe = Arc::new(Probe::new(&request, None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let result = checkpoint.trigger_authorized(
            CheckpointRequest::new(CheckpointBlockers::default(), probe),
            authority,
        );
        let error = match result {
            Ok(ticket) => runtime.block_on(ticket.wait()).unwrap_err(),
            Err(error) => error,
        };

        assert_eq!(
            error,
            CheckpointControlError::AuthorityDenied {
                stage: denied_stage
            }
        );
        assert_eq!(checkpoint.status(), CheckpointStatus::default());
        writer.shutdown_and_join().unwrap();
    }
}

#[test]
fn online_backup_is_verified_and_leaves_the_source_writer_usable() {
    let database = TestDatabase::new();
    let first = request(metadata("operation.backup.first", "key.backup.first", 'b'));
    let writer = start(&database, &first, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime
        .block_on(writer.submit(first.clone(), Arc::new(Probe::new(&first, None))))
        .unwrap();
    let destination = database.0.with_extension("backup.sqlite3");
    let allowed = Arc::new(AtomicBool::new(true));

    let receipt = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(ToggleAuthority {
                allowed: Arc::clone(&allowed),
            }),
        ))
        .unwrap();

    assert_eq!(
        receipt.source_watermark.commit_sequence,
        CommitSequenceV1(1)
    );
    assert!(receipt.destination_bytes > 0);
    assert_ne!(receipt.destination_sha256.0, [0; 32]);
    let backup_rows: i64 = Connection::open(&destination)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writer_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(backup_rows, 1);

    let second = request(metadata(
        "operation.backup.second",
        "key.backup.second",
        'c',
    ));
    runtime
        .block_on(writer.submit(second.clone(), Arc::new(Probe::new(&second, None))))
        .unwrap();
    let source_rows: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM writer_test", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_rows, 2);
    writer.shutdown_and_join().unwrap();
    std::fs::remove_file(destination).unwrap();
}

#[test]
fn online_backup_rejects_revoked_authority_and_existing_destinations() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.backup.reject",
        "key.backup.reject",
        'r',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let destination = database.0.with_extension("backup-reject.sqlite3");

    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(RevokeAfterAdmissionAuthority {
                admitted: AtomicBool::new(false),
            }),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::Dequeued
        }
    ));
    assert!(!destination.exists());

    std::fs::write(&destination, b"existing").unwrap();
    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(ToggleAuthority {
                allowed: Arc::new(AtomicBool::new(true)),
            }),
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::DestinationExists)
    ));
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
    writer.shutdown_and_join().unwrap();
    std::fs::remove_file(destination).unwrap();
}

#[test]
fn online_backup_authority_loss_before_publication_removes_staging() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.backup.prepublish",
        "key.backup.prepublish",
        'p',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let destination = database.0.with_extension("backup-prepublish.sqlite3");

    let error = runtime
        .block_on(writer.snapshot_to(
            destination.clone(),
            Arc::new(DenyThirdBeforeCommitAuthority {
                before_commit_checks: AtomicU64::new(0),
            }),
        ))
        .unwrap_err();

    assert!(matches!(
        error,
        WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::BeforeCommit
        }
    ));
    assert!(!destination.exists());
    let staging_prefix = format!(
        ".{}.tracedecay-backup-",
        destination.file_name().unwrap().to_string_lossy()
    );
    let leaked_staging = std::fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with(&staging_prefix))
        .collect::<Vec<_>>();
    assert!(leaked_staging.is_empty(), "{leaked_staging:?}");
    writer.shutdown_and_join().unwrap();
}

#[test]
fn online_backup_cancellation_and_deadline_remove_private_staging() {
    for interruption in [
        RuntimeInterruptionV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded,
    ] {
        let database = TestDatabase::new();
        let request = request(metadata(
            "operation.backup.interrupt",
            "key.backup.interrupt",
            'i',
        ));
        let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let destination = database.0.with_extension("backup-interrupted.sqlite3");
        let probe = Arc::new(DelayedInterruptionProbe {
            inner: Probe::new(&request, None),
            checks_before_interruption: AtomicU64::new(3),
            interruption,
        });

        let error = runtime
            .block_on(writer.snapshot_to_interruptible(
                destination.clone(),
                probe,
                Arc::new(ToggleAuthority {
                    allowed: Arc::new(AtomicBool::new(true)),
                }),
            ))
            .unwrap_err();

        assert!(matches!(
            (interruption, error),
            (
                RuntimeInterruptionV1::Cancelled,
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::Cancelled)
            ) | (
                RuntimeInterruptionV1::DeadlineExceeded,
                WriterActorError::OnlineBackupFailed(WriterOnlineBackupError::DeadlineExceeded)
            )
        ));
        assert!(!destination.exists());
        let staging_prefix = format!(
            ".{}.tracedecay-backup-",
            destination.file_name().unwrap().to_string_lossy()
        );
        assert!(
            std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&staging_prefix))
        );
        writer.shutdown_and_join().unwrap();
    }
}

#[test]
fn hard_checkpoint_pressure_emits_s5_general_admission_block() {
    let sample = WalSample {
        frames: 64,
        bytes: 256 * 1024 * 1024,
    };
    let blockers = CheckpointBlockers::default();
    let result = CheckpointResult::Decision {
        sample,
        decision: CheckpointDecision::Pending {
            mode: CheckpointMode::Passive,
            pressure: WalPressure::Hard,
            wal_bytes: sample.bytes,
            report: CheckpointReport {
                busy: false,
                log_frames: sample.frames,
                checkpointed_frames: sample.frames - 1,
            },
            snapshot_blockers: blockers.clone(),
            hard_drain_required: true,
            elapsed: Duration::ZERO,
        },
    };

    assert_eq!(
        worker::checkpoint_pressure_signal(&result),
        Some(CheckpointPressure::BlockGeneral {
            wal: crate::CheckpointWal::from_sample(sample),
            blockers,
        })
    );
}

#[test]
fn maintenance_checkpoint_uses_linear_permit_through_the_handle() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.maintenance-checkpoint",
        "key.maintenance-checkpoint",
        'm',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime
        .block_on(writer.submit(request.clone(), Arc::new(Probe::new(&request, None))))
        .unwrap();
    let permit = ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(1).unwrap(),
        writer.binding().clone(),
    );
    writer.begin_drain();

    let result = runtime
        .block_on(async {
            checkpoint
                .trigger_maintenance(MaintenanceCheckpointRequest::new(
                    MaintenanceCheckpointMode::Restart,
                    permit,
                    CheckpointBlockers::default(),
                ))
                .unwrap()
                .wait()
                .await
        })
        .unwrap();

    assert!(matches!(
        result,
        CheckpointOutcome::Complete {
            kind: CheckpointKind::Restart,
            ..
        }
    ));
    writer.shutdown_and_join().unwrap();
}

#[test]
fn maintenance_checkpoint_surfaces_blockers_without_faulting_writer() {
    let database = TestDatabase::new();
    let request = request(metadata(
        "operation.maintenance-blocked",
        "key.maintenance-blocked",
        'b',
    ));
    let writer = start(&database, &request, Arc::new(AtomicU64::new(0)));
    let checkpoint = writer.checkpoint_handle();
    let permit = ExclusiveMaintenancePermit::issue(
        MaintenanceOwnerId::new(1).unwrap(),
        writer.binding().clone(),
    );
    writer.begin_drain();
    let blockers = CheckpointBlockers {
        blockers: Vec::new(),
        omitted: 1,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let error = runtime
        .block_on(async {
            checkpoint
                .trigger_maintenance(MaintenanceCheckpointRequest::new(
                    MaintenanceCheckpointMode::Restart,
                    permit,
                    blockers.clone(),
                ))
                .unwrap()
                .wait()
                .await
        })
        .unwrap_err();

    assert_eq!(error, CheckpointControlError::Blocked(blockers));
    assert_eq!(writer.state(), WriterState::Draining);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn cancelled_before_admission_never_enters_the_queue() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.cancel", "key.cancel", 'c'));
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &request, Arc::clone(&applied));
    let probe = Arc::new(Probe::new(&request, Some(RuntimeInterruptionV1::Cancelled)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let outcome = runtime.block_on(writer.submit(request, probe)).unwrap();
    assert!(matches!(
        outcome,
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            stage: RuntimeCancellationStageV1::BeforeAdmission,
            ..
        }
    ));
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn cancelled_request_does_not_interrupt_an_unrelated_request_in_the_same_batch() {
    let database = TestDatabase::new();
    let first = request(metadata(
        "operation.cancel.batch.first",
        "key.cancel.batch.first",
        'c',
    ));
    let second = request(metadata(
        "operation.cancel.batch.second",
        "key.cancel.batch.second",
        'd',
    ));
    let binding = binding(&first.envelope().metadata);
    let first_probe = Arc::new(Probe::new(&first, None));
    let second_probe = Arc::new(Probe::new(&second, None));
    let admission = Admission::new(
        Limits::new(
            Capacity {
                operations: 2,
                bytes: u64::MAX,
            },
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            u64::MAX,
            u64::MAX,
        )
        .unwrap(),
    );
    let (first_reply, mut first_result) = tokio::sync::oneshot::channel();
    let (second_reply, mut second_result) = tokio::sync::oneshot::channel();
    let first = Arc::new(first);
    let second = Arc::new(second);
    let batch = request::ExecutionBatch {
        bytes: first.envelope().metadata.admission_bytes
            + second.envelope().metadata.admission_bytes,
        items: vec![
            AcceptedRequest::new(
                Arc::clone(&first),
                first_probe.clone(),
                Arc::new(UnrestrictedRuntimeWriteAuthority),
                first_reply,
                admission.reserve(&first.envelope().metadata).unwrap(),
            ),
            AcceptedRequest::new(
                Arc::clone(&second),
                second_probe,
                Arc::new(UnrestrictedRuntimeWriteAuthority),
                second_reply,
                admission.reserve(&second.envelope().metadata).unwrap(),
            ),
        ],
    };
    let mut connection = Connection::open(&database.0).unwrap();
    let telemetry = WriterTelemetry::default();
    let state = AtomicU8::new(WriterState::Ready as u8);
    let watermark = CommittedWatermarkPublisher::new(binding.clone());
    let mut persistence = CancellingFirstRequestPersistence {
        first_probe,
        sequence: 0,
    };

    worker::process_execution_batch(
        &mut connection,
        &binding,
        batch,
        &mut persistence,
        &telemetry,
        &state,
        &watermark,
    );

    assert!(matches!(
        first_result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. })
    ));
    assert!(matches!(
        second_result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::Committed { .. })
    ));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM cancellation_batch", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn active_long_running_request_remains_interruptible() {
    let database = TestDatabase::new();
    let request = request(metadata("operation.cancel.long", "key.cancel.long", 'e'));
    let binding = binding(&request.envelope().metadata);
    let probe = Arc::new(DelayedInterruptionProbe {
        inner: Probe::new(&request, None),
        checks_before_interruption: AtomicU64::new(1),
        interruption: RuntimeInterruptionV1::Cancelled,
    });
    let admission = Admission::new(
        Limits::new(
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            Capacity {
                operations: 1,
                bytes: u64::MAX,
            },
            u64::MAX,
            u64::MAX,
        )
        .unwrap(),
    );
    let (reply, mut result) = tokio::sync::oneshot::channel();
    let request = Arc::new(request);
    let batch = request::ExecutionBatch {
        bytes: request.envelope().metadata.admission_bytes,
        items: vec![AcceptedRequest::new(
            Arc::clone(&request),
            probe,
            Arc::new(UnrestrictedRuntimeWriteAuthority),
            reply,
            admission.reserve(&request.envelope().metadata).unwrap(),
        )],
    };
    let mut connection = Connection::open(&database.0).unwrap();
    let telemetry = WriterTelemetry::default();
    let state = AtomicU8::new(WriterState::Ready as u8);
    let watermark = CommittedWatermarkPublisher::new(binding.clone());

    worker::process_execution_batch(
        &mut connection,
        &binding,
        batch,
        &mut LongRunningPersistence,
        &telemetry,
        &state,
        &watermark,
    );

    assert!(matches!(
        result.try_recv().unwrap(),
        Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            stage: RuntimeCancellationStageV1::BeforeCommit,
            ..
        })
    ));
    assert_eq!(state.load(Ordering::SeqCst), WriterState::Ready as u8);
}

#[test]
fn queued_fact_write_rechecks_authority_before_opening_a_transaction() {
    let database = TestDatabase::new();
    let request = fact_request("operation.authority.queued", "key.authority.queued", 'q');
    let applied = Arc::new(AtomicU64::new(0));
    let writer = start(&database, &request, Arc::clone(&applied));
    let authority = Arc::new(RevokeAfterAdmissionAuthority {
        admitted: AtomicBool::new(false),
    });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let queued_outcome = runtime
        .block_on(writer.submit_authorized(request, probe, authority))
        .unwrap();

    assert_eq!(
        queued_outcome,
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::MissingAuthority,
        }
    );
    assert_eq!(applied.load(Ordering::SeqCst), 0);
    let table_count: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
    writer.shutdown_and_join().unwrap();
}

#[test]
fn queued_evidence_and_anchor_writes_recheck_authority_before_sql_dispatch() {
    let evidence = RepositoryWritePayloadV1::EvidenceAssembly(Box::new(
        crate::repository::evidence_assembly::tests::write_fixture("authority.test"),
    ));
    let anchor = RepositoryWritePayloadV1::RetrievalAnchorDisposition(Box::new(
        RetrievalAnchorDispositionRecordV1::new(
            "disposition.authority.fixture",
            tracedecay_domain::RetrievalAnchorId::new("retrieval.source.fixture").unwrap(),
            FactOwnerV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            },
            AnchorDispositionStateV1::Unavailable,
            None,
            AnchorDispositionReasonClassV1::SourceUnavailable,
            UtcMicros(1),
        )
        .unwrap(),
    ));

    for (label, payload, digest_byte) in [
        ("evidence", evidence, 'e'),
        ("retrieval_anchor", anchor, 'r'),
    ] {
        let database = TestDatabase::new();
        let request = project_fixture_request(
            &format!("operation.authority.{label}"),
            &format!("key.authority.{label}"),
            digest_byte,
            payload,
        );
        let applied = Arc::new(AtomicU64::new(0));
        let writer = start(&database, &request, Arc::clone(&applied));
        let authority = Arc::new(RevokeAfterAdmissionAuthority {
            admitted: AtomicBool::new(false),
        });
        let probe = Arc::new(Probe::new(&request, None));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let outcome = runtime
            .block_on(writer.submit_authorized(request, probe, authority))
            .unwrap();

        assert_eq!(
            outcome,
            RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::MissingAuthority,
            },
            "{label} write bypassed the actor authority recheck"
        );
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        let table_count: i64 = Connection::open(&database.0)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
        writer.shutdown_and_join().unwrap();
    }
}

#[test]
fn fact_write_rechecks_authority_before_outer_commit_and_rolls_back() {
    let database = TestDatabase::new();
    let request = fact_request(
        "operation.authority.precommit",
        "key.authority.precommit",
        'p',
    );
    let applied = Arc::new(AtomicU64::new(0));
    let allowed = Arc::new(AtomicBool::new(true));
    let writer = start_with_persistence(
        &database,
        &request,
        Box::new(RevokingPersistence {
            inner: TestPersistence {
                applied: Arc::clone(&applied),
                sequence: 0,
            },
            allowed: Arc::clone(&allowed),
        }),
    );
    let authority = Arc::new(ToggleAuthority { allowed });
    let probe = Arc::new(Probe::new(&request, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    let outcome = runtime
        .block_on(writer.submit_authorized(request, probe, authority))
        .unwrap();

    assert_eq!(
        outcome,
        RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::MissingAuthority,
        }
    );
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    let table_count: i64 = Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'writer_test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
    writer.shutdown_and_join().unwrap();
}
