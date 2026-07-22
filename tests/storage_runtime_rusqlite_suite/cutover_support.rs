#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, Savepoint, Transaction};
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    reader::{ExistingReaderLocator, ReaderQueryExecutor},
};
use tracedecay_store::{
    AdmissionConfigV1, LocatorDigest, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdentityV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StorageRuntimeErrorV1,
    StoreOperationMetadataV1, StoreRuntimeBindingV1, TransactionalOutboxEntryV1,
    VerifiedStoreLocatorV1,
};

const FIXTURE: &str = include_str!("../fixtures/storage_runtime_cutover/cutover-v1.json");

#[derive(Debug, Deserialize)]
pub(crate) struct CutoverFixture {
    pub(crate) s5: S5Fixture,
    pub(crate) s6: S6Fixture,
    pub(crate) s7: S7Fixture,
    pub(crate) s8: S8Fixture,
    pub(crate) s9: S9Fixture,
    pub(crate) s10: S10Fixture,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S5Fixture {
    pub(crate) binding: StoreRuntimeBindingV1,
    pub(crate) reader_budget: ReaderBudgetFixture,
    pub(crate) initial_commit_sequence: u64,
    pub(crate) published_commit_sequence: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReaderBudgetFixture {
    pub(crate) min_per_hot_shard: u16,
    pub(crate) max_per_hot_shard: u16,
    pub(crate) idle_burst_retire_ms: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S6Fixture {
    pub(crate) maintenance_telemetry: tracedecay_store::MaintenanceTelemetryV1,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S7Fixture {
    pub(crate) worktree_binding: StoreRuntimeBindingV1,
    pub(crate) snapshot_binding: StoreRuntimeBindingV1,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S8Fixture {
    pub(crate) families: Vec<RepositoryFamilyFixture>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RepositoryFamilyFixture {
    pub(crate) family: String,
    pub(crate) write_payloads: Vec<String>,
    pub(crate) read_operations: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S9Fixture {
    pub(crate) origin_binding: StoreRuntimeBindingV1,
    pub(crate) target_binding: StoreRuntimeBindingV1,
    pub(crate) effect_id: String,
    pub(crate) ordering_key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct S10Fixture {
    pub(crate) effect_id: String,
    pub(crate) ordering_key: String,
    pub(crate) commit_sequences: Vec<u64>,
}

pub(crate) fn fixture() -> CutoverFixture {
    serde_json::from_str(FIXTURE).expect("storage-runtime cutover fixture must decode")
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
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let count = snapshot
            .query_row("SELECT COUNT(*) FROM acceptance_rows", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
                operation: format!("read acceptance row count: {error}"),
            })?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::GraphQuickCheck { healthy: count > 0 }),
            RuntimeReadCoverageV1::Latest { observed: None },
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
        "operation": { "kind": "graph_quick_check" },
        "priority": priority,
        "admission_bytes": 64,
        "control": {
            "requested_at": 1,
            "deadline": { "deadline_id": format!("deadline.cutover.{priority}") },
            "cancellation": {
                "cancellation_id": format!("cancellation.cutover.{priority}"),
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
        "client_id": "client.cutover.acceptance",
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
            "CREATE TABLE IF NOT EXISTS cutover_writes (
                operation INTEGER PRIMARY KEY AUTOINCREMENT
            );
            INSERT INTO cutover_writes DEFAULT VALUES;",
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
            "CREATE TABLE IF NOT EXISTS cutover_effects (
                effect_json TEXT NOT NULL
            );",
        )?;
        savepoint.execute(
            "INSERT INTO cutover_effects(effect_json) VALUES (?1)",
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
