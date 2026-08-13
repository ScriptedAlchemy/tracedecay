//! Real, no-mock integration coverage for `StorageOperationExecutor::apply_inbox`
//! (`crates/tracedecay-rusqlite-runtime/src/operation.rs`), the transactional
//! inbox write path.
//!
//! `apply_inbox` is a default method on the crate's public
//! `StorageOperationExecutor` trait: it validates nothing itself and simply
//! forwards the closed `ApplyInbox` payload to `self.execute(..)` inside the
//! writer's request savepoint (see `operation.rs`). The dispatch that reaches
//! it — `operation::execute` matching `RepositoryWritePayloadV1::ApplyInbox`
//! and calling `executor.apply_inbox(..)`, itself driven by
//! `RuntimeWriterPersistence::apply_and_record` from `persistence.rs` — is
//! `pub(crate)`, so the only way to exercise `apply_inbox` end to end without
//! reaching into crate-private internals is through the crate's public writer
//! actor, `PersistentWriter`. That is also the most faithful test: it is
//! exactly how a real caller reaches this code.
//!
//! These tests submit `RepositoryWritePayloadV1::ApplyInbox` requests through
//! a `PersistentWriter` backed by a real on-disk SQLite file (via
//! `ExistingWriterLocator`) and a custom `StorageOperationExecutor` — not a
//! mock, a small real executor that performs real SQL against the real
//! savepoint it is handed, following the same pattern as the existing
//! `MarkerExecutor` test doubles in `tests/runtime_actor/support.rs`.

use std::sync::Arc;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor, WriterActorError,
    WriterState,
};
use tracedecay_store::{
    AdmissionConfigV1, LocatorDigest, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdentityV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, RuntimeTransactionIdV1, RuntimeTransactionScopeV1,
    StoreOperationMetadataV1, TransactionalOutboxEntryV1, VerifiedStoreLocatorV1,
};

const MARKER_TABLE: &str = "td_apply_inbox_marker_v1";

struct TestDatabase {
    _directory: TempDir,
    path: std::path::PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create isolated writer directory");
        let path = directory.path().join("runtime.db");
        std::fs::File::create(&path).expect("create existing verified store");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connect(&self) -> Connection {
        Connection::open(&self.path).expect("reopen runtime store")
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

/// A real (non-mock) `StorageOperationExecutor`: it performs a genuine SQL
/// insert against the savepoint `apply_inbox` hands it, keyed by the applied
/// entry's effect id, so tests can observe exactly how many times the native
/// apply actually ran. When `fail` is set it returns an error *after* the
/// insert, so a rollback that leaves the marker behind would be a real bug,
/// not a passing test by omission.
struct ApplyInboxMarkerExecutor {
    fail: bool,
}

impl StorageOperationExecutor for ApplyInboxMarkerExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        let RepositoryWritePayloadV1::ApplyInbox(entry) = payload else {
            panic!("this test only ever submits ApplyInbox payloads");
        };
        savepoint.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {MARKER_TABLE} (effect_id TEXT PRIMARY KEY NOT NULL)"
        ))?;
        savepoint.execute(
            &format!("INSERT INTO {MARKER_TABLE}(effect_id) VALUES (?1)"),
            [entry.identity.effect_id.as_str()],
        )?;
        if self.fail {
            return Err(rusqlite::Error::InvalidParameterName(
                "injected apply-inbox failure".to_owned(),
            ));
        }
        Ok(())
    }
}

fn marker_count(database: &TestDatabase) -> i64 {
    let connection = database.connect();
    let exists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [MARKER_TABLE],
            |row| row.get(0),
        )
        .unwrap();
    if exists == 0 {
        0
    } else {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {MARKER_TABLE}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

fn request(project: &str, operation_id: &str, key: &str, digest_byte: char) -> RuntimeSubmitRequestV1 {
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(serde_json::json!({
        "operation_id": operation_id,
        "client_id": format!("client.{project}"),
        "shard_id": {
            "brain_id": "brain.inbox",
            "profile_id": "profile.inbox",
            "scope": { "kind": "project", "project_id": project }
        },
        "incarnation": 1,
        "authority_epoch": 7,
        "idempotency": { "key": key, "command_digest": digest(digest_byte) },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 128,
        "admitted_at": 1
    }))
    .expect("valid inbox metadata");
    // `apply_inbox` commits an effect that is landing at *this* writer's own
    // binding: the ledger's inbox bookkeeping (`ledger::inbox::validate_target`
    // in `src/ledger/inbox.rs`, driven from `commit::inbox_receipt` in
    // `src/ledger/commit.rs`) requires `identity.target_watermark` to equal
    // the writer's `(shard_id, incarnation, authority_epoch)` binding — i.e.
    // `metadata.shard_id` here — and requires `state == Dispatched` (an
    // inbox apply always represents an already-dispatched source effect
    // landing at its target). `source_watermark` must name a *different*
    // shard than the target (see `EffectIdentityV1::validate`), so the
    // source side reuses the project-sessions scope instead.
    let target_shard = serde_json::to_value(&metadata.shard_id).unwrap();
    let entry: TransactionalOutboxEntryV1 = serde_json::from_value(serde_json::json!({
        "identity": {
            "effect_id": format!("effect.{operation_id}"),
            "command_digest": digest('e'),
            "ordering_key": format!("{project}.inbox"),
            "source_watermark": {
                "shard_id": {
                    "brain_id": "brain.inbox",
                    "profile_id": "profile.inbox",
                    "scope": { "kind": "project_sessions", "project_id": project }
                },
                "incarnation": 1,
                "authority_epoch": 7,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": target_shard,
                "incarnation": 1,
                "authority_epoch": 7,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "dispatched",
        "acknowledgement": null,
        "enqueued_at": 1,
        "updated_at": 1
    }))
    .expect("valid inbox outbox entry");
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.{operation_id}")).unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata).unwrap(),
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(serde_json::json!({
        "requested_at": 1,
        "deadline": { "deadline_id": format!("deadline.{operation_id}") },
        "cancellation": {
            "cancellation_id": format!("cancellation.{operation_id}"),
            "generation": 1
        }
    }))
    .unwrap();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::ApplyInbox(Box::new(entry)),
        },
        transaction_scope,
        control,
    )
    .unwrap()
}

struct FixedProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl FixedProbe {
    fn for_request(request: &RuntimeSubmitRequestV1) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
        })
    }
}

impl RuntimeRequestProbeV1 for FixedProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        true
    }
}

fn writer(database: &TestDatabase, request: &RuntimeSubmitRequestV1, fail: bool) -> PersistentWriter {
    let binding = request.binding().clone();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(digest('d')).unwrap(),
    );
    PersistentWriter::start(
        ExistingWriterLocator::new(binding, locator, database.path.clone()).unwrap(),
        AdmissionConfigV1::default(),
        ApplyInboxMarkerExecutor { fail },
    )
    .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[test]
fn apply_inbox_commits_the_native_effect_against_a_real_sqlite_store() {
    let database = TestDatabase::new();
    let request = request(
        "project.inbox.success",
        "operation.inbox.success",
        "key.inbox.success",
        'a',
    );
    let writer = writer(&database, &request, false);
    let outcome = runtime()
        .block_on(writer.submit(request.clone(), FixedProbe::for_request(&request)))
        .unwrap();
    assert!(matches!(outcome, RuntimeSubmitOutcomeV1::Committed { .. }));
    writer.shutdown_and_join().unwrap();

    assert_eq!(marker_count(&database), 1);
    let stored_effect_id: String = database
        .connect()
        .query_row(&format!("SELECT effect_id FROM {MARKER_TABLE}"), [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored_effect_id, "effect.operation.inbox.success");
}

#[test]
fn replaying_the_same_inbox_submission_does_not_double_apply() {
    let database = TestDatabase::new();
    let request = request(
        "project.inbox.replay",
        "operation.inbox.replay",
        "key.inbox.replay",
        'a',
    );
    let writer = writer(&database, &request, false);
    let rt = runtime();

    let first = rt
        .block_on(writer.submit(request.clone(), FixedProbe::for_request(&request)))
        .unwrap();
    let first_receipt = match first {
        RuntimeSubmitOutcomeV1::Committed { receipt } => receipt,
        other => panic!("expected the first submission to commit, got {other:?}"),
    };

    // Same operation id, same idempotency key and command digest: a genuine
    // replay of the exact same request, not a new logical submission.
    let second = rt
        .block_on(writer.submit(request.clone(), FixedProbe::for_request(&request)))
        .unwrap();
    let second_receipt = match second {
        RuntimeSubmitOutcomeV1::ExactReplay { receipt } => receipt,
        other => panic!("expected the replay to be recognized as an exact replay, got {other:?}"),
    };
    assert_eq!(
        first_receipt, second_receipt,
        "a replay must resolve to the exact receipt the original apply produced"
    );

    writer.shutdown_and_join().unwrap();
    assert_eq!(
        marker_count(&database),
        1,
        "the native apply_inbox effect must run exactly once, not once per replayed submission"
    );
}

#[test]
fn a_failing_apply_leaves_no_partial_effect_after_rollback() {
    let database = TestDatabase::new();
    let request = request(
        "project.inbox.failure",
        "operation.inbox.failure",
        "key.inbox.failure",
        'a',
    );
    let writer = writer(&database, &request, true);
    let outcome = runtime().block_on(writer.submit(request.clone(), FixedProbe::for_request(&request)));
    assert!(
        matches!(outcome, Err(WriterActorError::StorageFailure(_))),
        "a native apply failure must surface as a storage failure, not silently succeed"
    );
    assert_eq!(
        writer.state(),
        WriterState::Ready,
        "a non-corrupt apply failure must not fault the whole writer"
    );
    writer.shutdown_and_join().unwrap();

    assert_eq!(
        marker_count(&database),
        0,
        "the request savepoint must roll back the marker insert that ran before the injected error"
    );
}
