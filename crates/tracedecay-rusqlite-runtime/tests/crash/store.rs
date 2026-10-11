use std::{path::Path, sync::Arc};

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    BrainId, CodeGenerationId, LocatorDigest, ProjectId, UserProfileId, UtcMicros,
    test_fixtures::id,
};
use tracedecay_rusqlite_runtime::{
    CheckpointBlockers, CheckpointRequest, RuntimeWriteAuthority, RuntimeWriteAuthorityError,
    RuntimeWriteAuthorityStage,
    repository::{
        RepositoryDispatchError, RepositoryPhysicalAttachmentFactory,
        RepositoryRuntimePhysicalAttachment,
    },
};
use tracedecay_sql::{Connection, OptionalExtension};
use tracedecay_store::{
    AdmissionConfigV1, GENERATION_DIAGNOSTICS_SCHEMA_DDL, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, SanitizedCleanDiagnosticSnapshotV1,
    ShardWatermarkV1, StoreIncarnationV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1, WAL_HARD_LIMIT_BYTES, WalBudgetV1,
};

pub fn binding() -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": StoreShardIdV1::project(
            id::<BrainId>("brain.crash"),
            id::<UserProfileId>("profile.crash"),
            id::<ProjectId>("project.crash"),
        ),
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .unwrap()
}

pub fn key(index: usize) -> String {
    format!("crash.key.{index}")
}

pub fn generation(index: usize) -> String {
    format!("generation.crash.{index}")
}

fn sha256(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

/// Request `index` is fully determined by its index, so a resubmission after
/// a crash is the same command under the same idempotency key.
pub fn request(index: usize) -> RuntimeSubmitRequestV1 {
    let key = key(index);
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(serde_json::json!({
        "operation_id": format!("crash.operation.{index}"),
        "client_id": "client.crash",
        "shard_id": binding().shard_id,
        "incarnation": 1,
        "authority_epoch": 1,
        "idempotency": { "key": key, "command_digest": sha256(&key) },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 128,
        "admitted_at": 1
    }))
    .unwrap();
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.crash.{index}")).unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata).unwrap(),
        opened_at: metadata.admitted_at,
    };
    let (cancellation, deadline) = identities(&format!("request.{index}"));
    let snapshot = SanitizedCleanDiagnosticSnapshotV1::new(
        CodeGenerationId::new(generation(index)).unwrap(),
        Vec::new(),
    )
    .unwrap();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::Diagnostics(Box::new(snapshot)),
        },
        transaction_scope,
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline,
            cancellation,
        },
    )
    .unwrap()
}

fn identities(name: &str) -> (RuntimeCancellationIdentityV1, RuntimeDeadlineV1) {
    (
        RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!("cancel.{name}")).unwrap(),
            generation: 1,
        },
        RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{name}")).unwrap(),
        },
    )
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl Probe {
    fn new(name: &str) -> Arc<Self> {
        let (cancellation, deadline) = identities(name);
        Arc::new(Self {
            cancellation,
            deadline,
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

    fn try_begin_commit(&self) -> bool {
        true
    }
}

struct Authorized;

impl RuntimeWriteAuthority for Authorized {
    fn verify(&self, _stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError> {
        Ok(())
    }
}

/// Creates the store file with the diagnostics schema the payload writes.
pub fn create_writer_store(path: &Path) {
    Connection::open(path)
        .unwrap()
        .execute_batch(GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
}

pub fn create_driver_store(path: &Path) {
    Connection::open(path)
        .unwrap()
        .execute_batch("CREATE TABLE crash_rows (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
        .unwrap();
}

/// Large enough that every row spans pages, so one commit writes many frames.
pub fn row_payload(index: usize) -> String {
    format!("row-{index}-").repeat(400)
}

/// Opens the registered writer and readers through the production factory.
pub fn attach(path: &Path) -> RepositoryRuntimePhysicalAttachment {
    let binding = binding();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).unwrap(),
        LocatorDigest::new(sha256("crash.locator")).unwrap(),
    );
    RepositoryPhysicalAttachmentFactory
        .attach(
            binding,
            locator,
            path.to_owned(),
            AdmissionConfigV1 {
                // Any WAL is over the soft limit, so every requested
                // checkpoint reaches the driver instead of the 32 MiB default
                // leaving it a no-op.
                wal: WalBudgetV1 {
                    soft_limit_bytes: 1,
                    hard_limit_bytes: WAL_HARD_LIMIT_BYTES,
                },
                ..AdmissionConfigV1::default()
            },
        )
        .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

pub fn submit(
    attachment: &RepositoryRuntimePhysicalAttachment,
    index: usize,
) -> Result<RuntimeSubmitOutcomeV1, RepositoryDispatchError> {
    runtime().block_on(attachment.dispatch_submit(
        request(index),
        Probe::new(&format!("request.{index}")),
        Arc::new(Authorized),
    ))
}

pub fn checkpoint(attachment: &RepositoryRuntimePhysicalAttachment) {
    runtime()
        .block_on(attachment.run_checkpoint(
            CheckpointRequest::new(CheckpointBlockers::default(), Probe::new("checkpoint")),
            Arc::new(Authorized),
        ))
        .unwrap();
}

/// What a reopened writer store holds durably.
#[derive(Debug)]
pub struct WriterDurable {
    pub integrity: String,
    /// `(commit_sequence, idempotency_key)` in sequence order.
    pub receipts: Vec<(u64, String)>,
    /// `(commit_sequence, watermark commit_sequence)` per checkpoint row.
    pub checkpoints: Vec<(u64, u64)>,
    pub publications: u64,
    pub current_generation: Option<String>,
}

fn has_table(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

fn integrity(connection: &Connection) -> String {
    connection
        .query_row("PRAGMA integrity_check", (), |row| row.get::<_, String>(0))
        .unwrap()
}

pub fn inspect_writer(path: &Path) -> WriterDurable {
    let connection = Connection::open(path).unwrap();
    let mut durable = WriterDurable {
        integrity: integrity(&connection),
        receipts: Vec::new(),
        checkpoints: Vec::new(),
        publications: connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostic_generation_publications",
                (),
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64,
        current_generation: connection
            .query_row(
                "SELECT generation_id FROM diagnostic_generation_publications \
                 WHERE record_state = 'current'",
                (),
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap(),
    };
    if has_table(&connection, "td_runtime_writer_idempotency_v2") {
        let mut statement = connection
            .prepare(
                "SELECT commit_sequence, idempotency_key FROM td_runtime_writer_idempotency_v2 \
                 ORDER BY commit_sequence",
            )
            .unwrap();
        durable.receipts = statement
            .query_map((), |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
    }
    if has_table(&connection, "td_runtime_writer_checkpoint_v1") {
        let mut statement = connection
            .prepare("SELECT commit_sequence, watermark_json FROM td_runtime_writer_checkpoint_v1")
            .unwrap();
        durable.checkpoints = statement
            .query_map((), |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|(sequence, json)| {
                let watermark: ShardWatermarkV1 = serde_json::from_str(&json).unwrap();
                (sequence, watermark.commit_sequence.0)
            })
            .collect();
    }
    durable
}

/// `(integrity_check, [(id, payload)])` of a reopened driver store.
pub fn inspect_driver(path: &Path) -> (String, Vec<(u64, String)>) {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT id, payload FROM crash_rows ORDER BY id")
        .unwrap();
    let rows = statement
        .query_map((), |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(statement);
    (integrity(&connection), rows)
}
