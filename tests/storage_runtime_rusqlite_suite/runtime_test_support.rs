#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, Savepoint, Transaction};
use serde_json::json;
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ExistingReaderLocator, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, CommitSequenceV1, LocatorDigest, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    RuntimeSubmitRequestV1, RuntimeTransactionIdV1, RuntimeTransactionScopeV1, ShardWatermarkV1,
    StorageRuntimeErrorV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
    TransactionalOutboxEntryV1, VerifiedStoreLocatorV1,
};

pub(crate) struct ReaderRuntimeFixture {
    pub(crate) binding: StoreRuntimeBindingV1,
    pub(crate) reader_budget: ReaderBudgetFixture,
    pub(crate) initial_commit_sequence: u64,
    pub(crate) published_commit_sequence: u64,
}

pub(crate) struct ReaderBudgetFixture {
    pub(crate) min_per_hot_shard: u16,
    pub(crate) max_per_hot_shard: u16,
    pub(crate) idle_burst_retire_ms: u64,
}

pub(crate) struct WriterRuntimeFixture {
    pub(crate) origin_binding: StoreRuntimeBindingV1,
    pub(crate) target_binding: StoreRuntimeBindingV1,
    pub(crate) effect_id: &'static str,
    pub(crate) ordering_key: &'static str,
    pub(crate) commit_sequences: [u64; 2],
}

pub(crate) fn reader_runtime_fixture() -> ReaderRuntimeFixture {
    ReaderRuntimeFixture {
        binding: serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.runtime-reader",
                "profile_id": "profile.runtime-reader",
                "scope": { "kind": "project", "project_id": "project.runtime-reader" }
            },
            "incarnation": 1,
            "authority_epoch": 7
        }))
        .expect("construct reader runtime binding"),
        reader_budget: ReaderBudgetFixture {
            min_per_hot_shard: 2,
            max_per_hot_shard: 2,
            idle_burst_retire_ms: 30_000,
        },
        initial_commit_sequence: 4,
        published_commit_sequence: 5,
    }
}

pub(crate) fn maintenance_binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(json!({
        "shard_id": {
            "brain_id": "brain.runtime-maintenance",
            "profile_id": "profile.runtime-maintenance",
            "scope": { "kind": "project", "project_id": "project.runtime-maintenance" }
        },
        "incarnation": 3,
        "authority_epoch": 11
    }))
    .expect("construct maintenance runtime binding")
}

pub(crate) fn writer_runtime_fixture() -> WriterRuntimeFixture {
    WriterRuntimeFixture {
        origin_binding: serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.runtime-writer",
                "profile_id": "profile.runtime-writer",
                "scope": { "kind": "project", "project_id": "project.runtime-writer-origin" }
            },
            "incarnation": 5,
            "authority_epoch": 13
        }))
        .expect("construct writer origin binding"),
        target_binding: serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.runtime-writer",
                "profile_id": "profile.runtime-writer",
                "scope": { "kind": "project_sessions", "project_id": "project.runtime-writer-origin" }
            },
            "incarnation": 6,
            "authority_epoch": 17
        }))
        .expect("construct writer target binding"),
        effect_id: "effect.runtime.writer",
        ordering_key: "project.runtime-writer.serialized",
        commit_sequences: [1, 2],
    }
}

pub(crate) struct TestDatabase {
    _root: TempDir,
    pub(crate) path: PathBuf,
}

impl TestDatabase {
    pub(crate) fn new(name: &str) -> Self {
        let root = tempfile::tempdir().expect("create storage-runtime acceptance root");
        let path = root.path().join(name);
        fs::File::create(&path).expect("create existing SQLite authority");
        Self { _root: root, path }
    }

    pub(crate) fn connect(&self) -> Connection {
        Connection::open(&self.path).expect("open acceptance SQLite authority")
    }
}

pub(crate) fn verified_locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(format!("sha256:{}", "d".repeat(64)))
            .expect("valid acceptance locator digest"),
    )
}

pub(crate) fn reader_locator(
    binding: &StoreRuntimeBindingV1,
    path: &Path,
) -> ExistingReaderLocator {
    ExistingReaderLocator::new(
        binding.clone(),
        verified_locator(binding),
        path.to_path_buf(),
    )
    .expect("valid existing reader locator")
}

pub(crate) fn writer(database: &TestDatabase, binding: &StoreRuntimeBindingV1) -> PersistentWriter {
    writer_with_executor(database, binding, NoopRepositoryWrite)
}

pub(crate) fn writer_with_executor<E>(
    database: &TestDatabase,
    binding: &StoreRuntimeBindingV1,
    executor: E,
) -> PersistentWriter
where
    E: StorageOperationExecutor + Send + 'static,
{
    let locator = writer_locator(database, binding);
    PersistentWriter::start(locator, AdmissionConfigV1::default(), executor)
        .expect("start persistent acceptance writer")
}

pub(crate) fn writer_locator(
    database: &TestDatabase,
    binding: &StoreRuntimeBindingV1,
) -> ExistingWriterLocator {
    ExistingWriterLocator::new(
        binding.clone(),
        verified_locator(binding),
        database.path.clone(),
    )
    .expect("valid existing writer locator")
}

#[derive(Clone, Copy)]
pub(crate) struct CountExecutor;

impl ReaderQueryExecutor for CountExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let count = snapshot
            .query_row("SELECT COUNT(*) FROM acceptance_rows", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("read acceptance row count: {error}"),
            })?;
        let watermark = ShardWatermarkV1 {
            shard_id: request.binding().shard_id.clone(),
            incarnation: request.binding().incarnation,
            authority_epoch: request.binding().authority_epoch,
            commit_sequence: CommitSequenceV1(if count > 0 { 1 } else { 0 }),
        };
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::CurrentWatermark {
                watermark: watermark.clone(),
            }),
            RuntimeReadCoverageV1::Latest {
                observed: Some(watermark),
            },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: format!("construct acceptance read outcome: {error}"),
        })
    }
}

pub(crate) struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl Probe {
    pub(crate) fn for_read(request: &RuntimeReadRequestV1) -> Self {
        Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
        }
    }

    pub(crate) fn for_submit(request: &RuntimeSubmitRequestV1) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
        })
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
        None
    }
}

pub(crate) fn read_request(
    binding: &StoreRuntimeBindingV1,
    priority: &str,
) -> RuntimeReadRequestV1 {
    serde_json::from_value(json!({
        "binding": binding,
        "consistency": { "kind": "latest_available" },
        "operation": { "kind": "current_watermark" },
        "priority": priority,
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": format!("deadline.runtime.{priority}") },
            "cancellation": {
                "cancellation_id": format!("cancellation.runtime.{priority}"),
                "generation": 1
            }
        }
    }))
    .expect("valid acceptance read request")
}

pub(crate) fn outbox_request(
    binding: &StoreRuntimeBindingV1,
    target: &StoreRuntimeBindingV1,
    operation_id: &str,
    effect_id: &str,
    ordering_key: &str,
) -> RuntimeSubmitRequestV1 {
    let digest = format!("sha256:{}", "a".repeat(64));
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(json!({
        "operation_id": operation_id,
        "client_id": "client.runtime.acceptance",
        "shard_id": binding.shard_id,
        "incarnation": binding.incarnation,
        "authority_epoch": binding.authority_epoch,
        "idempotency": {
            "key": format!("key.{operation_id}"),
            "command_digest": digest
        },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 256,
        "admitted_at": 1
    }))
    .expect("valid acceptance operation metadata");
    let source_shard = serde_json::to_value(&binding.shard_id).expect("encode source shard");
    let target_shard = serde_json::to_value(&target.shard_id).expect("encode target shard");
    let outbox: TransactionalOutboxEntryV1 = serde_json::from_value(json!({
        "identity": {
            "effect_id": effect_id,
            "command_digest": format!("sha256:{}", "e".repeat(64)),
            "ordering_key": ordering_key,
            "source_watermark": {
                "shard_id": source_shard,
                "incarnation": binding.incarnation,
                "authority_epoch": binding.authority_epoch,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": target_shard,
                "incarnation": target.incarnation,
                "authority_epoch": target.authority_epoch,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "pending",
        "acknowledgement": null,
        "enqueued_at": 1,
        "updated_at": 1
    }))
    .expect("valid acceptance outbox entry");
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.{operation_id}"))
            .expect("valid acceptance transaction id"),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata)
            .expect("compatible acceptance transaction"),
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(json!({
        "requested_at": 1,
        "deadline": { "deadline_id": format!("deadline.{operation_id}") },
        "cancellation": {
            "cancellation_id": format!("cancellation.{operation_id}"),
            "generation": 1
        }
    }))
    .expect("valid acceptance request control");
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::EnqueueOutbox(Box::new(outbox)),
        },
        transaction_scope,
        control,
    )
    .expect("valid acceptance submit request")
}

#[derive(Clone, Copy)]
struct NoopRepositoryWrite;

impl StorageOperationExecutor for NoopRepositoryWrite {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        savepoint.execute_batch(
            "CREATE TABLE IF NOT EXISTS runtime_writes (
                operation INTEGER PRIMARY KEY AUTOINCREMENT
            );
            INSERT INTO runtime_writes DEFAULT VALUES;",
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RecordingEffect;

impl StorageOperationExecutor for RecordingEffect {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }

    fn apply_inbox(
        &mut self,
        savepoint: &Savepoint<'_>,
        entry: &TransactionalOutboxEntryV1,
    ) -> rusqlite::Result<()> {
        savepoint.execute_batch(
            "CREATE TABLE IF NOT EXISTS runtime_effects (
                effect_json TEXT NOT NULL
            );",
        )?;
        savepoint.execute(
            "INSERT INTO runtime_effects(effect_json) VALUES (?1)",
            [serde_json::to_string(&entry.effect)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?],
        )?;
        Ok(())
    }
}

pub(crate) fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build acceptance runtime")
        .block_on(future)
}
