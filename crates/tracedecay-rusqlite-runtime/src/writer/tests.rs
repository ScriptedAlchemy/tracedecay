use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, Savepoint, Transaction};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, IdempotencyIdentityV1, LocatorDigest,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StorageRuntimeErrorV1, StoreCommitReceiptV1,
    StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::*;
use crate::{
    checkpoint::{
        CheckpointBlockers, CheckpointDecision, CheckpointInterruption, CheckpointKind,
        CheckpointMode, CheckpointOutcome, CheckpointPressure, CheckpointReport, CheckpointResult,
        MaintenanceCheckpointMode, WalPressure, WalSample,
    },
    maintenance::{ExclusiveMaintenancePermit, MaintenanceOwnerId},
    test_support::{binding, metadata, request},
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
