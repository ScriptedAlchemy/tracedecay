//! Real-file contention through production writer actors and repository SQL.
//!
//! Concurrent submitters on one shard queue behind its single writer. Distinct
//! shard files have distinct actors; readers use WAL snapshots. The
//! `storage_contention` bench target measures the same fixtures.

use std::sync::{Arc, Barrier, Condvar, Mutex, mpsc};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_domain::test_fixtures::{id, repeated_sha256_text as digest};
use tracedecay_domain::{
    DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, GenerationDiagnosticV1, SourceSpan, UtcMicros,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ReaderPool, ReaderQueryExecutor},
    repository::{ConcreteRepositoryReadExecutor, ConcreteRepositoryWriteExecutor},
};
use tracedecay_sql::{Savepoint, Transaction};
use tracedecay_store::{
    AdmissionConfigV1, DiagnosticReadOperationV1, DiagnosticReadResultV1,
    GENERATION_DIAGNOSTICS_SCHEMA_DDL, ProjectReadOperationV1, ProjectReadResultV1,
    RepositoryReadOperationV1, RepositoryReadResultV1, RepositoryWritePayloadV1,
    RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    SanitizedCleanDiagnosticSnapshotV1, StorageRuntimeErrorV1, StoreCommitReceiptV1,
    StoreRuntimeBindingV1,
};

use crate::runtime_test_support::{
    Probe, TestDatabase, outbox_request, read_request, reader_locator, run, verified_locator,
    writer_locator, writer_runtime_fixture,
};

pub(crate) fn binding(shard: usize) -> StoreRuntimeBindingV1 {
    let mut value = serde_json::to_value(writer_runtime_fixture().origin_binding).unwrap();
    value["shard_id"]["scope"]["project_id"] = format!("project.contention.{shard}").into();
    serde_json::from_value(value).unwrap()
}

fn record(generation: &str, ordinal: usize) -> GenerationDiagnosticV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id(&format!("anchor.{generation}.{ordinal}")),
        generation_id: id(generation),
        repository: id("repository.contention"),
        worktree: Some(id("worktree.contention")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.contention")),
        file_occurrence_id: id("file.contention"),
        content_digest: id(&digest('a')),
        span: SourceSpan {
            start_byte: 10,
            end_byte: 42,
        },
        symbol_occurrence_id: None,
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: id(&digest('b')),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id("producer.rustc"),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            sanitization_receipt: Some(id("receipt.sanitization")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(1_700_000_000_000_000),
        state: DiagnosticRecordStateV1::Current,
    };
    record.message_digest = record.compute_message_digest().unwrap();
    record
}

pub(crate) fn publication(
    binding: &StoreRuntimeBindingV1,
    name: &str,
    records: usize,
) -> RuntimeSubmitRequestV1 {
    let target = writer_runtime_fixture().target_binding;
    let base = outbox_request(
        binding,
        &target,
        name,
        &format!("effect.{name}"),
        "contention",
    );
    let mut envelope = base.envelope().clone();
    let records: Vec<_> = (0..records).map(|ordinal| record(name, ordinal)).collect();
    let command_bytes =
        serde_json::to_vec(&serde_json::json!({ "generation_id": name, "records": records }))
            .unwrap();
    envelope.metadata.admission_bytes = u64::try_from(command_bytes.len()).unwrap();
    let command_digest = Sha256::digest(&command_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    envelope.metadata.idempotency.command_digest =
        tracedecay_store::CommandDigestV1::new(format!("sha256:{command_digest}")).unwrap();
    envelope.payload = RepositoryWritePayloadV1::Diagnostics(Box::new(
        SanitizedCleanDiagnosticSnapshotV1::new(id(name), records).unwrap(),
    ));
    RuntimeSubmitRequestV1::new(
        envelope,
        base.transaction_scope().clone(),
        base.control().clone(),
    )
    .unwrap()
}

pub(crate) fn seed(database: &TestDatabase) {
    database
        .connect()
        .execute_batch(GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
}

pub(crate) fn start_writer(
    database: &TestDatabase,
    binding: &StoreRuntimeBindingV1,
    executor: impl StorageOperationExecutor + Send + 'static,
) -> PersistentWriter {
    PersistentWriter::start(
        writer_locator(database, binding),
        AdmissionConfigV1::default(),
        executor,
    )
    .unwrap()
}

pub(crate) fn commit(
    writer: &PersistentWriter,
    request: RuntimeSubmitRequestV1,
) -> StoreCommitReceiptV1 {
    let probe = Probe::for_submit(&request);
    match run(writer.submit(request, probe)).unwrap() {
        RuntimeSubmitOutcomeV1::Committed { receipt } => receipt,
        outcome => panic!("expected commit, got {outcome:?}"),
    }
}

pub(crate) fn repository_read(binding: &StoreRuntimeBindingV1) -> RuntimeReadRequestV1 {
    let mut value = serde_json::to_value(read_request(binding, "foreground")).unwrap();
    value["operation"] = serde_json::to_value(RuntimeReadOperationV1::Repository {
        op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Diagnostics(
            DiagnosticReadOperationV1::CurrentGeneration,
        )),
    })
    .unwrap();
    serde_json::from_value(value).unwrap()
}

#[derive(Clone, Default)]
pub(crate) struct RepositoryReader(ConcreteRepositoryReadExecutor);

impl ReaderQueryExecutor for RepositoryReader {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let RuntimeReadOperationV1::Repository { op } = request.operation() else {
            panic!("repository read required")
        };
        let result = self.0.execute(snapshot, op).map_err(|error| {
            StorageRuntimeErrorV1::Infrastructure {
                operation: format!("contention repository read: {error}"),
            }
        })?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::Repository { result }),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: error.to_string(),
        })
    }
}

pub(crate) fn readers(
    database: &TestDatabase,
    binding: &StoreRuntimeBindingV1,
) -> ReaderPool<RepositoryReader> {
    ReaderPool::start(
        reader_locator(binding, &database.path),
        AdmissionConfigV1::default().readers,
        RepositoryReader::default(),
    )
    .unwrap()
}

pub(crate) fn generation(outcome: &RuntimeReadOutcomeV1) -> String {
    match outcome.value() {
        Some(RuntimeReadResultV1::Repository {
            result: RepositoryReadResultV1::Project(project),
        }) => match project.as_ref() {
            ProjectReadResultV1::Diagnostics(DiagnosticReadResultV1::CurrentGeneration(Some(
                generation,
            ))) => generation.as_str().to_owned(),
            other => panic!("expected current generation, got {other:?}"),
        },
        other => panic!("expected repository read, got {other:?}"),
    }
}

/// Rows durably present in `table`. The writer creates its ledger tables inside
/// the first committed transaction, so a rolled-back first write leaves none.
fn persisted_rows(connection: &tracedecay_sql::Connection, table: &str) -> i64 {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return 0;
    }
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

pub(crate) fn durable_sequences(database: &TestDatabase, expected: u64) {
    let connection = database.connect();
    let mut statement = connection
        .prepare(
            "SELECT commit_sequence FROM td_runtime_writer_idempotency_v2 ORDER BY commit_sequence",
        )
        .unwrap();
    let actual: Vec<i64> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let expected = i64::try_from(expected).expect("test sequence fits SQLite signed integers");
    assert_eq!(actual, (1..=expected).collect::<Vec<_>>());
    let checkpoint: i64 = connection
        .query_row(
            "SELECT commit_sequence FROM td_runtime_writer_checkpoint_v1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(checkpoint, expected);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

/// Pauses after the concrete repository mutation, inside its request savepoint.
struct PausedPublication {
    entered: mpsc::SyncSender<()>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl StorageOperationExecutor for PausedPublication {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> tracedecay_sql::Result<()> {
        ConcreteRepositoryWriteExecutor::default().execute(savepoint, payload)?;
        self.entered.send(()).unwrap();
        let (released, condition) = &*self.gate;
        let (released, timeout) = condition
            .wait_timeout_while(
                released.lock().unwrap(),
                Duration::from_secs(10),
                |released| !*released,
            )
            .unwrap();
        assert!(
            *released && !timeout.timed_out(),
            "publication release timed out"
        );
        Ok(())
    }
}

struct OpenGateOnDrop(Arc<(Mutex<bool>, Condvar)>);
impl Drop for OpenGateOnDrop {
    fn drop(&mut self) {
        let (released, condition) = &*self.0;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        condition.notify_all();
    }
}

#[test]
fn concurrent_wal_readers_keep_snapshots_while_a_repository_write_commits() {
    let database = TestDatabase::new("concurrent-readers.db");
    seed(&database);
    let binding = binding(0);
    let initial = start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    );
    commit(&initial, publication(&binding, "generation.initial", 4));
    initial.shutdown_and_join().unwrap();
    let pool = readers(&database, &binding);
    let request = repository_read(&binding);
    let probe = Probe::for_read(&request);
    let mut lease_a = pool.acquire(&request, &probe, Duration::ZERO).unwrap();
    let mut lease_b = pool.acquire(&request, &probe, Duration::ZERO).unwrap();
    let mut snapshot_a = lease_a.begin_snapshot().unwrap();
    let mut snapshot_b = lease_b.begin_snapshot().unwrap();
    assert_eq!(
        generation(&snapshot_a.execute(request.clone(), &probe).unwrap()),
        "generation.initial"
    );
    assert_eq!(
        generation(&snapshot_b.execute(request.clone(), &probe).unwrap()),
        "generation.initial"
    );

    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let writer = Arc::new(start_writer(
        &database,
        &binding,
        PausedPublication {
            entered: entered_tx,
            gate: Arc::clone(&gate),
        },
    ));
    let release = OpenGateOnDrop(Arc::clone(&gate));
    std::thread::scope(|scope| {
        let writer = Arc::clone(&writer);
        let binding = &binding;
        let submission =
            scope.spawn(move || commit(&writer, publication(binding, "generation.next", 4)));
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Both dedicated readers execute while the writer has already mutated
        // this same file inside BEGIN IMMEDIATE, before its COMMIT.
        assert_eq!(
            generation(&snapshot_a.execute(request.clone(), &probe).unwrap()),
            "generation.initial"
        );
        assert_eq!(
            generation(&snapshot_b.execute(request.clone(), &probe).unwrap()),
            "generation.initial"
        );
        drop(release);
        assert_eq!(submission.join().unwrap().commit_sequence.0, 2);
    });
    assert_eq!(
        generation(&snapshot_a.execute(request.clone(), &probe).unwrap()),
        "generation.initial"
    );
    assert_eq!(
        generation(&snapshot_b.execute(request.clone(), &probe).unwrap()),
        "generation.initial"
    );
    drop(snapshot_a);
    drop(snapshot_b);
    let mut fresh = lease_a.begin_snapshot().unwrap();
    assert_eq!(
        generation(&fresh.execute(request.clone(), &probe).unwrap()),
        "generation.next"
    );
    drop(fresh);
    drop(lease_a);
    drop(lease_b);
    drop(pool);
    Arc::try_unwrap(writer)
        .ok()
        .unwrap()
        .shutdown_and_join()
        .unwrap();
    durable_sequences(&database, 2);
}

#[test]
fn competing_submissions_preserve_contiguous_receipts_after_reopen_and_conflict() {
    let database = TestDatabase::new("competing-submissions.db");
    seed(&database);
    let binding = binding(0);
    let writer = Arc::new(start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    ));
    let barrier = Barrier::new(5);
    let receipts = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..4)
            .map(|producer| {
                let writer = Arc::clone(&writer);
                let binding = &binding;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    (0..8)
                        .map(|operation| {
                            commit(
                                &writer,
                                publication(
                                    binding,
                                    &format!("generation.producer.{producer}.{operation}"),
                                    2,
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        barrier.wait();
        tasks
            .into_iter()
            .flat_map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    Arc::try_unwrap(writer)
        .ok()
        .unwrap()
        .shutdown_and_join()
        .unwrap();
    let mut sequences: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.commit_sequence.0)
        .collect();
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=32).collect::<Vec<_>>());
    durable_sequences(&database, 32);
    let restarted = start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    );
    let original = &receipts[0];
    let replay = publication(&binding, original.operation_id.as_str(), 2);
    assert_eq!(
        run(restarted.submit(replay.clone(), Probe::for_submit(&replay))).unwrap(),
        RuntimeSubmitOutcomeV1::ExactReplay {
            receipt: original.clone()
        }
    );
    let mut envelope = replay.envelope().clone();
    envelope.metadata.idempotency.command_digest =
        tracedecay_store::CommandDigestV1::new(digest('b')).unwrap();
    let conflict = RuntimeSubmitRequestV1::new(
        envelope,
        replay.transaction_scope().clone(),
        replay.control().clone(),
    )
    .unwrap();
    assert_eq!(
        run(restarted.submit(conflict.clone(), Probe::for_submit(&conflict))).unwrap(),
        RuntimeSubmitOutcomeV1::IdempotencyConflict {
            existing_receipt: original.clone()
        }
    );
    assert_eq!(
        commit(
            &restarted,
            publication(&binding, "generation.after-reopen", 2)
        )
        .commit_sequence
        .0,
        33
    );
    restarted.shutdown_and_join().unwrap();
    durable_sequences(&database, 33);
}

#[test]
fn conflicting_concurrent_submissions_execute_only_the_winning_publication() {
    let database = TestDatabase::new("concurrent-conflicts.db");
    seed(&database);
    let binding = binding(0);
    let writer = Arc::new(start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    ));
    let first = publication(&binding, "generation.conflict.first", 4);
    let second = publication(&binding, "generation.conflict.second", 4);
    let mut envelope = second.envelope().clone();
    envelope.metadata.idempotency.key = first.envelope().metadata.idempotency.key.clone();
    let second = RuntimeSubmitRequestV1::new(
        envelope,
        second.transaction_scope().clone(),
        second.control().clone(),
    )
    .unwrap();
    let barrier = Barrier::new(3);
    let outcomes = std::thread::scope(|scope| {
        let tasks: Vec<_> = [first, second]
            .into_iter()
            .map(|request| {
                let writer = Arc::clone(&writer);
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    run(writer.submit(request.clone(), Probe::for_submit(&request))).unwrap()
                })
            })
            .collect();
        barrier.wait();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    let receipts: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            RuntimeSubmitOutcomeV1::Committed { receipt } => Some(receipt),
            _ => None,
        })
        .collect();
    assert_eq!(
        receipts.len(),
        1,
        "one idempotency key must commit once: {outcomes:?}"
    );
    assert!(outcomes.iter().any(|outcome| matches!(outcome, RuntimeSubmitOutcomeV1::IdempotencyConflict { existing_receipt } if existing_receipt == receipts[0])));
    Arc::try_unwrap(writer)
        .ok()
        .unwrap()
        .shutdown_and_join()
        .unwrap();
    let connection = database.connect();
    let generations: Vec<String> = connection
        .prepare("SELECT generation_id FROM diagnostic_generation_publications")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(generations, [receipts[0].operation_id.as_str()]);
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM generation_diagnostics", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 4, "losing publication must leave no domain rows");
    durable_sequences(&database, 1);
}

struct FailOnceAfterMutation(bool);
impl StorageOperationExecutor for FailOnceAfterMutation {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> tracedecay_sql::Result<()> {
        ConcreteRepositoryWriteExecutor::default().execute(savepoint, payload)?;
        if std::mem::take(&mut self.0) {
            return Err(tracedecay_sql::Error::InvalidQuery);
        }
        Ok(())
    }
}

#[test]
fn failed_repository_mutation_rolls_back_and_the_same_request_can_retry() {
    let database = TestDatabase::new("rollback-retry.db");
    seed(&database);
    let binding = binding(0);
    let writer = start_writer(&database, &binding, FailOnceAfterMutation(true));
    let request = publication(&binding, "generation.retry", 4);
    assert!(run(writer.submit(request.clone(), Probe::for_submit(&request))).is_err());
    for table in [
        "generation_diagnostics",
        "diagnostic_generation_publications",
        "td_runtime_writer_checkpoint_v1",
        "td_runtime_writer_idempotency_v2",
    ] {
        assert_eq!(
            persisted_rows(&database.connect(), table),
            0,
            "failed mutation leaked rows into {table}"
        );
    }
    assert_eq!(commit(&writer, request).commit_sequence.0, 1);
    writer.shutdown_and_join().unwrap();
    durable_sequences(&database, 1);
}

#[test]
fn competing_sqlite_writer_lock_returns_an_error_and_resubmit_commits_once() {
    let database = TestDatabase::new("external-write-lock.db");
    seed(&database);
    let binding = binding(0);
    let writer = start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    );
    let connection = database.connect();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let request = publication(&binding, "generation.lock-retry", 4);
    assert!(run(writer.submit(request.clone(), Probe::for_submit(&request))).is_err());
    assert_eq!(writer.telemetry_snapshot().busy_events, 1);
    assert_eq!(
        persisted_rows(&connection, "td_runtime_writer_idempotency_v2"),
        0
    );
    connection.execute_batch("ROLLBACK").unwrap();
    let receipt = commit(&writer, request.clone());
    assert_eq!(receipt.commit_sequence.0, 1);
    assert_eq!(
        run(writer.submit(request.clone(), Probe::for_submit(&request))).unwrap(),
        RuntimeSubmitOutcomeV1::ExactReplay { receipt }
    );
    writer.shutdown_and_join().unwrap();
    durable_sequences(&database, 1);
}

struct ExitAfterMutation;
impl StorageOperationExecutor for ExitAfterMutation {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> tracedecay_sql::Result<()> {
        ConcreteRepositoryWriteExecutor::default().execute(savepoint, payload)?;
        if matches!(payload, RepositoryWritePayloadV1::Diagnostics(snapshot) if snapshot.generation_id().as_str() == "generation.crashing")
        {
            // Exit the whole process with a live SQLite transaction, before
            // COMMIT or actor/destructor cleanup can make it durable.
            std::process::exit(86);
        }
        Ok(())
    }
}

#[test]
#[ignore = "subprocess entry point for the real-file crash recovery test"]
fn crash_writer_process() {
    let path = std::env::var_os("TRACEDECAY_CRASH_DATABASE").expect("crash subprocess database");
    let binding = binding(0);
    let writer = PersistentWriter::start(
        ExistingWriterLocator::new(binding.clone(), verified_locator(&binding), path.into())
            .unwrap(),
        AdmissionConfigV1::default(),
        ExitAfterMutation,
    )
    .unwrap();
    assert_eq!(
        commit(&writer, publication(&binding, "generation.before-crash", 4))
            .commit_sequence
            .0,
        1
    );
    commit(&writer, publication(&binding, "generation.crashing", 4));
    panic!("crash executor must exit during the second mutation");
}

#[test]
fn process_crash_preserves_committed_receipts_and_discards_uncommitted_mutations() {
    let database = TestDatabase::new("process-crash.db");
    seed(&database);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage_contention::crash_writer_process",
            "--ignored",
            "--nocapture",
        ])
        .env("TRACEDECAY_CRASH_DATABASE", &database.path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(86),
        "crash subprocess did not reach its open transaction: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    durable_sequences(&database, 1);
    let count: i64 = database.connect().query_row(
        "SELECT COUNT(*) FROM generation_diagnostics WHERE generation_id = 'generation.crashing'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0, "uncommitted domain rows survived process crash");
    let binding = binding(0);
    let restarted = start_writer(
        &database,
        &binding,
        ConcreteRepositoryWriteExecutor::default(),
    );
    let previous = publication(&binding, "generation.before-crash", 4);
    assert!(
        matches!(run(restarted.submit(previous.clone(), Probe::for_submit(&previous))).unwrap(), RuntimeSubmitOutcomeV1::ExactReplay { receipt } if receipt.commit_sequence.0 == 1)
    );
    assert_eq!(
        commit(&restarted, publication(&binding, "generation.crashing", 4))
            .commit_sequence
            .0,
        2
    );
    restarted.shutdown_and_join().unwrap();
    durable_sequences(&database, 2);
}
