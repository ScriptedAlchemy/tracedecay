use super::{LedgerError, RequestSql};
use crate::writer::{NativeSubmission, NativeWriteExecutor, NativeWriter};
use crate::{Access, Database, ExecutionGuard, TransactionBehavior, Value};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tracedecay_store::{
    CommandDigestV1, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeRequestControlV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreOperationMetadataV1,
    StoreRuntimeBindingV1, TransactionalOutboxEntryV1,
};
use tracedecay_store::{
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1,
};

fn digest(byte: char) -> CommandDigestV1 {
    CommandDigestV1::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn metadata(operation_id: &str, key: &str, digest_byte: char) -> StoreOperationMetadataV1 {
    serde_json::from_value(serde_json::json!({
        "operation_id": operation_id,
        "client_id": "client.runtime",
        "shard_id": {
            "brain_id": "brain.runtime",
            "profile_id": "profile.runtime",
            "scope": { "kind": "project", "project_id": "project.runtime" }
        },
        "incarnation": 1,
        "authority_epoch": 7,
        "idempotency": { "key": key, "command_digest": digest(digest_byte) },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 128,
        "admitted_at": 1
    }))
    .unwrap()
}

fn scope(metadata: &StoreOperationMetadataV1) -> RuntimeTransactionScopeV1 {
    RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(metadata).unwrap(),
        opened_at: metadata.admitted_at,
    }
}

fn binding(metadata: &StoreOperationMetadataV1) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        metadata.shard_id.clone(),
        metadata.incarnation,
        metadata.authority_epoch,
    )
}

fn outbox(metadata: &StoreOperationMetadataV1) -> TransactionalOutboxEntryV1 {
    serde_json::from_value(serde_json::json!({
        "identity": {
            "effect_id": format!("effect.{}", metadata.operation_id.as_str()),
            "command_digest": digest('e'),
            "ordering_key": "project.runtime.observations",
            "source_watermark": {
                "shard_id": metadata.shard_id,
                "incarnation": metadata.incarnation,
                "authority_epoch": metadata.authority_epoch,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": {
                    "brain_id": "brain.runtime",
                    "profile_id": "profile.runtime",
                    "scope": { "kind": "project_sessions", "project_id": "project.runtime" }
                },
                "incarnation": 1,
                "authority_epoch": 7,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "pending",
        "acknowledgement": null,
        "enqueued_at": 1,
        "updated_at": 1
    }))
    .unwrap()
}

fn request(metadata: StoreOperationMetadataV1) -> RuntimeSubmitRequestV1 {
    let transaction_scope = scope(&metadata);
    let entry = outbox(&metadata);
    let control: RuntimeRequestControlV1 = serde_json::from_value(serde_json::json!({
        "requested_at": 1,
        "deadline": { "deadline_id": "deadline.runtime" },
        "cancellation": { "cancellation_id": "cancellation.runtime", "generation": 1 }
    }))
    .unwrap();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::EnqueueOutbox(Box::new(entry)),
        },
        transaction_scope,
        control,
    )
    .unwrap()
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    cancelled: Arc<AtomicBool>,
    claimed: AtomicBool,
    deny_commit: bool,
}
impl Probe {
    fn new(request: &RuntimeSubmitRequestV1, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            cancelled,
            claimed: AtomicBool::new(false),
            deny_commit: false,
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
        self.cancelled
            .load(Ordering::Acquire)
            .then_some(RuntimeInterruptionV1::Cancelled)
    }
    fn try_begin_commit(&self) -> bool {
        !self.deny_commit
            && !self.cancelled.load(Ordering::Acquire)
            && !self.claimed.swap(true, Ordering::AcqRel)
    }
}
#[derive(Default)]
struct Marker;
impl NativeWriteExecutor for Marker {
    fn execute(
        &mut self,
        sql: &RequestSql<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), LedgerError> {
        let RepositoryWritePayloadV1::EnqueueOutbox(entry) = payload else {
            return Err(crate::Error::Unsupported(payload.name().into()).into());
        };
        sql.execute(
            "INSERT INTO domain_marker(value) VALUES (?1)",
            &[Value::Text(entry.identity.effect_id.as_str().into())],
        )?;
        Ok(())
    }
}
fn guard() -> ExecutionGuard {
    ExecutionGuard::new(None, Arc::new(AtomicBool::new(false)), None)
}
fn setup(database: &Database, guard: &ExecutionGuard) {
    database
        .connect(Access::Writer)
        .unwrap()
        .execute(
            "CREATE TABLE domain_marker(value TEXT NOT NULL)",
            &[],
            guard,
        )
        .unwrap();
}
fn count(database: &Database, table: &str, guard: &ExecutionGuard) -> i64 {
    let mut reader = database.connect(Access::Reader).unwrap();
    let rows = reader
        .query(&format!("SELECT COUNT(*) FROM {table}"), &[], guard)
        .unwrap();
    let Value::Integer(count) = rows.values[0][0] else {
        panic!("integer count")
    };
    count
}

#[test]
fn native_writer_batches_savepoints_receipts_outbox_replay_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("writer.db");
    let guard = guard();
    let database = Database::open(&path).unwrap();
    setup(&database, &guard);
    let first = request(metadata("operation.first", "key.first", 'a'));
    let third = request(metadata("operation.third", "key.third", 'c'));
    let mut collision_entry = outbox(&first.envelope().metadata);
    collision_entry.identity.effect_id = match &first.envelope().payload {
        RepositoryWritePayloadV1::EnqueueOutbox(entry) => entry.identity.effect_id.clone(),
        _ => unreachable!(),
    };
    let mut middle_envelope = first.envelope().clone();
    middle_envelope.metadata = metadata("operation.middle", "key.middle", 'b');
    middle_envelope.payload = RepositoryWritePayloadV1::EnqueueOutbox(Box::new(collision_entry));
    let middle = RuntimeSubmitRequestV1::new(
        middle_envelope.clone(),
        scope(&middle_envelope.metadata),
        first.control().clone(),
    )
    .unwrap();
    let binding = first.binding().clone();
    let mut writer = NativeWriter::new(&database, binding.clone(), &guard).unwrap();
    let probes =
        [&first, &middle, &third].map(|request| Probe::new(request, Arc::clone(&guard.cancelled)));
    let submissions = [&first, &middle, &third]
        .into_iter()
        .zip(&probes)
        .map(|(request, probe)| NativeSubmission {
            request,
            guard: &guard,
            probe,
        })
        .collect::<Vec<_>>();
    let outcomes = writer
        .process_batch(&submissions, &guard, &mut Marker)
        .unwrap();
    let RuntimeSubmitOutcomeV1::Committed {
        receipt: first_receipt,
    } = outcomes[0].as_ref().unwrap()
    else {
        panic!("first committed")
    };
    let RuntimeSubmitOutcomeV1::Committed {
        receipt: third_receipt,
    } = outcomes[2].as_ref().unwrap()
    else {
        panic!("third committed")
    };
    assert_eq!(first_receipt.commit_sequence.0, 1);
    assert!(outcomes[1].is_err());
    assert_eq!(third_receipt.commit_sequence.0, 2);
    assert_eq!(count(&database, "domain_marker", &guard), 2);
    assert_eq!(count(&database, "td_runtime_writer_outbox_v1", &guard), 2);
    assert_eq!(
        count(&database, "td_runtime_writer_idempotency_v2", &guard),
        2
    );
    let first_receipt = first_receipt.clone();
    let replay = request(metadata("operation.replay", "key.first", 'a'));
    let conflict = request(metadata("operation.conflict", "key.first", 'b'));
    let replay_probe = Probe::new(&replay, Arc::clone(&guard.cancelled));
    let conflict_probe = Probe::new(&conflict, Arc::clone(&guard.cancelled));
    let outcomes = writer
        .process_batch(
            &[
                NativeSubmission {
                    request: &replay,
                    guard: &guard,
                    probe: &replay_probe,
                },
                NativeSubmission {
                    request: &conflict,
                    guard: &guard,
                    probe: &conflict_probe,
                },
            ],
            &guard,
            &mut Marker,
        )
        .unwrap();
    assert!(
        matches!(&outcomes[0], Ok(RuntimeSubmitOutcomeV1::ExactReplay { receipt }) if *receipt == first_receipt)
    );
    assert!(
        matches!(&outcomes[1], Ok(RuntimeSubmitOutcomeV1::IdempotencyConflict { existing_receipt }) if *existing_receipt == first_receipt)
    );
    assert_eq!(count(&database, "domain_marker", &guard), 2);
    drop(writer);
    drop(database);
    let database = Database::open(&path).unwrap();
    let mut writer = NativeWriter::new(&database, binding, &guard).unwrap();
    assert_eq!(
        writer
            .current_watermark(&guard)
            .unwrap()
            .unwrap()
            .commit_sequence
            .0,
        2
    );
    assert_eq!(
        writer
            .lookup_receipt(&first.envelope().metadata.idempotency, &guard)
            .unwrap(),
        Some(first_receipt)
    );
    let mut reader = database.connect(Access::Reader).unwrap();
    assert_eq!(
        reader
            .query("PRAGMA integrity_check", &[], &guard)
            .unwrap()
            .values,
        vec![vec![Value::Text("ok".into())]]
    );
}

#[test]
fn native_writer_commit_arbitration_rolls_back_every_domain_and_ledger_row() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("rollback.db")).unwrap();
    let guard = guard();
    setup(&database, &guard);
    let request = request(metadata("operation.cancel", "key.cancel", 'a'));
    let mut probe = Probe::new(&request, Arc::clone(&guard.cancelled));
    probe.deny_commit = true;
    let mut writer = NativeWriter::new(&database, request.binding().clone(), &guard).unwrap();
    assert!(
        writer
            .process_batch(
                &[NativeSubmission {
                    request: &request,
                    guard: &guard,
                    probe: &probe
                }],
                &guard,
                &mut Marker
            )
            .is_err()
    );
    for table in [
        "domain_marker",
        "td_runtime_writer_checkpoint_v1",
        "td_runtime_writer_idempotency_v2",
        "td_runtime_writer_outbox_v1",
        "td_runtime_writer_inbox_v1",
    ] {
        assert_eq!(count(&database, table, &guard), 0);
    }
    let probe = Probe::new(&request, Arc::clone(&guard.cancelled));
    let outcomes = writer
        .process_batch(
            &[NativeSubmission {
                request: &request,
                guard: &guard,
                probe: &probe,
            }],
            &guard,
            &mut Marker,
        )
        .unwrap();
    assert!(
        matches!(&outcomes[0], Ok(RuntimeSubmitOutcomeV1::Committed { receipt }) if receipt.commit_sequence.0 == 1)
    );
}

#[test]
fn native_ledger_rejects_corrupted_checkpoint_before_publishing_next_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("corrupt.db")).unwrap();
    let guard = guard();
    setup(&database, &guard);
    let first = request(metadata("operation.first", "key.first", 'a'));
    let probe = Probe::new(&first, Arc::clone(&guard.cancelled));
    let mut writer = NativeWriter::new(&database, first.binding().clone(), &guard).unwrap();
    writer
        .process_batch(
            &[NativeSubmission {
                request: &first,
                guard: &guard,
                probe: &probe,
            }],
            &guard,
            &mut Marker,
        )
        .unwrap();
    let mut connection = database.connect(Access::Writer).unwrap();
    connection
        .execute(
            "UPDATE td_runtime_writer_checkpoint_v1 SET commit_sequence = 9",
            &[],
            &guard,
        )
        .unwrap();
    let second = request(metadata("operation.second", "key.second", 'b'));
    let probe = Probe::new(&second, Arc::clone(&guard.cancelled));
    assert!(matches!(
        writer.process_batch(
            &[NativeSubmission {
                request: &second,
                guard: &guard,
                probe: &probe
            }],
            &guard,
            &mut Marker
        ),
        Err(LedgerError::Corrupt { .. })
    ));
    assert_eq!(count(&database, "domain_marker", &guard), 1);
    assert_eq!(
        count(&database, "td_runtime_writer_idempotency_v2", &guard),
        1
    );
}

#[test]
fn native_ledger_receipt_and_outbox_share_transaction_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("atomic.db")).unwrap();
    let guard = guard();
    let metadata = metadata("operation.atomic", "key.atomic", 'a');
    let binding = binding(&metadata);
    let entry = outbox(&metadata);
    let mut connection = database.connect(Access::Writer).unwrap();
    connection
        .begin(TransactionBehavior::Immediate, &guard)
        .unwrap();
    {
        let sql = RequestSql::new(&mut connection, &guard);
        super::initialize_writer_ledger(&sql).unwrap();
        super::commit::record_commit(&sql, &metadata, &scope(&metadata), Some(&entry)).unwrap();
        assert!(
            super::inbox::lookup(&sql, &binding, &entry)
                .unwrap()
                .is_none()
        );
    }
    connection.rollback().unwrap();
    let sql = RequestSql::new(&mut connection, &guard);
    super::initialize_writer_ledger(&sql).unwrap();
    assert!(super::current_watermark(&sql, &binding).unwrap().is_none());
    assert!(
        super::outbox::outbox_entry(&sql, &binding, &entry.identity.effect_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn native_writer_executes_closed_diagnostics_with_same_boundary_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("diagnostic-writer.db")).unwrap();
    let guard = guard();
    database
        .connect(Access::Writer)
        .unwrap()
        .execute_batch(tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL, &guard)
        .unwrap();
    let original = request(metadata("operation.diagnostics", "key.diagnostics", 'a'));
    let mut envelope = original.envelope().clone();
    envelope.payload = RepositoryWritePayloadV1::Diagnostics(Box::new(
        tracedecay_store::SanitizedCleanDiagnosticSnapshotV1::new(
            tracedecay_domain::CodeGenerationId::new("generation.native").unwrap(),
            vec![],
        )
        .unwrap(),
    ));
    let request = RuntimeSubmitRequestV1::new(
        envelope,
        original.transaction_scope().clone(),
        original.control().clone(),
    )
    .unwrap();
    let probe = Probe::new(&request, Arc::clone(&guard.cancelled));
    let mut writer = NativeWriter::new(&database, request.binding().clone(), &guard).unwrap();
    let result = writer
        .process_batch(
            &[NativeSubmission {
                request: &request,
                guard: &guard,
                probe: &probe,
            }],
            &guard,
            &mut crate::diagnostics::NativeDiagnosticExecutor,
        )
        .unwrap();
    assert!(
        matches!(&result[0], Ok(RuntimeSubmitOutcomeV1::Committed { receipt }) if receipt.commit_sequence.0 == 1)
    );
    assert_eq!(
        count(&database, "diagnostic_generation_publications", &guard),
        1
    );
    assert_eq!(
        writer
            .lookup_receipt(&request.envelope().metadata.idempotency, &guard)
            .unwrap()
            .unwrap()
            .commit_sequence
            .0,
        1
    );
}

#[test]
fn native_request_cancellation_rolls_back_only_its_savepoint_before_peer_commit() {
    struct CancelFirst {
        cancelled: Arc<AtomicBool>,
        calls: usize,
    }
    impl NativeWriteExecutor for CancelFirst {
        fn execute(
            &mut self,
            sql: &RequestSql<'_>,
            payload: &RepositoryWritePayloadV1,
        ) -> Result<(), LedgerError> {
            Marker.execute(sql, payload)?;
            if self.calls == 0 {
                self.cancelled.store(true, Ordering::Release);
            }
            self.calls += 1;
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(&directory.path().join("cancel-peer.db")).unwrap();
    let batch_guard = guard();
    let cancelled_guard = guard();
    let peer_guard = guard();
    setup(&database, &batch_guard);
    let first = request(metadata("operation.cancel", "key.cancel", 'a'));
    let peer = request(metadata("operation.peer", "key.peer", 'b'));
    let first_probe = Probe::new(&first, Arc::clone(&cancelled_guard.cancelled));
    let peer_probe = Probe::new(&peer, Arc::clone(&peer_guard.cancelled));
    let mut writer = NativeWriter::new(&database, first.binding().clone(), &batch_guard).unwrap();
    let outcomes = writer
        .process_batch(
            &[
                NativeSubmission {
                    request: &first,
                    guard: &cancelled_guard,
                    probe: &first_probe,
                },
                NativeSubmission {
                    request: &peer,
                    guard: &peer_guard,
                    probe: &peer_probe,
                },
            ],
            &batch_guard,
            &mut CancelFirst {
                cancelled: Arc::clone(&cancelled_guard.cancelled),
                calls: 0,
            },
        )
        .unwrap();
    assert!(matches!(
        &outcomes[0],
        Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. })
    ));
    assert!(
        matches!(&outcomes[1], Ok(RuntimeSubmitOutcomeV1::Committed { receipt }) if receipt.commit_sequence.0 == 1)
    );
    assert_eq!(count(&database, "domain_marker", &batch_guard), 1);
    assert!(
        writer
            .lookup_receipt(&first.envelope().metadata.idempotency, &batch_guard)
            .unwrap()
            .is_none()
    );
}
