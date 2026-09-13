use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
};
use tracedecay_store::{
    AdmissionConfigV1, LocatorDigest, OperationPriorityV1, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    RuntimeSubmitRequestV1, RuntimeTransactionIdV1, RuntimeTransactionScopeV1,
    StoreOperationMetadataV1, TransactionalOutboxEntryV1, VerifiedStoreLocatorV1,
};

const MARKER_TABLE: &str = "td_runtime_actor_marker";

pub(crate) struct TestDatabase {
    _directory: TempDir,
    pub(crate) path: std::path::PathBuf,
}

impl TestDatabase {
    pub(crate) fn new() -> Self {
        let directory = tempfile::tempdir().expect("create isolated writer directory");
        let path = directory.path().join("runtime.db");
        std::fs::File::create(&path).expect("create existing verified store");
        Self {
            _directory: directory,
            path,
        }
    }

    pub(crate) fn connect(&self) -> Connection {
        Connection::open(&self.path).expect("reopen runtime store")
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TestBinding {
    pub(crate) project: &'static str,
    pub(crate) incarnation: u64,
    pub(crate) authority_epoch: u64,
}

impl TestBinding {
    pub(crate) const fn project(project: &'static str) -> Self {
        Self {
            project,
            incarnation: 1,
            authority_epoch: 7,
        }
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn priority_name(priority: OperationPriorityV1) -> &'static str {
    match priority {
        OperationPriorityV1::Health => "health",
        OperationPriorityV1::Foreground => "foreground",
        OperationPriorityV1::Background => "background",
    }
}

pub(crate) fn request(
    binding: TestBinding,
    operation_id: &str,
    key: &str,
    digest_byte: char,
    priority: OperationPriorityV1,
) -> RuntimeSubmitRequestV1 {
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(serde_json::json!({
        "operation_id": operation_id,
        "client_id": format!("client.{}", binding.project),
        "shard_id": {
            "brain_id": "brain.actor",
            "profile_id": "profile.actor",
            "scope": { "kind": "project", "project_id": binding.project }
        },
        "incarnation": binding.incarnation,
        "authority_epoch": binding.authority_epoch,
        "idempotency": { "key": key, "command_digest": digest(digest_byte) },
        "durability": "full",
        "priority": priority_name(priority),
        "admission_bytes": 128,
        "admitted_at": 1
    }))
    .expect("valid actor metadata");
    let source_shard = serde_json::to_value(&metadata.shard_id).unwrap();
    let outbox: TransactionalOutboxEntryV1 = serde_json::from_value(serde_json::json!({
        "identity": {
            "effect_id": format!("effect.{operation_id}"),
            "command_digest": digest('e'),
            "ordering_key": format!("{}.actor", binding.project),
            "source_watermark": {
                "shard_id": source_shard,
                "incarnation": binding.incarnation,
                "authority_epoch": binding.authority_epoch,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": {
                    "brain_id": "brain.actor",
                    "profile_id": "profile.actor",
                    "scope": { "kind": "project_sessions", "project_id": binding.project }
                },
                "incarnation": binding.incarnation,
                "authority_epoch": binding.authority_epoch,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "pending",
        "acknowledgement": null,
        "enqueued_at": 1,
        "updated_at": 1
    }))
    .expect("valid actor outbox");
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
            payload: RepositoryWritePayloadV1::EnqueueOutbox(Box::new(outbox)),
        },
        transaction_scope,
        control,
    )
    .unwrap()
}

#[derive(Clone, Default)]
pub(crate) struct ExecutorControl {
    pub(crate) entered: Option<mpsc::SyncSender<()>>,
    pub(crate) release: Option<Arc<(Mutex<bool>, Condvar)>>,
    pub(crate) after_mutation: Option<LifecycleBarrier>,
    pub(crate) panic_after_mutation: bool,
}

struct MarkerExecutor {
    control: ExecutorControl,
}

impl StorageOperationExecutor for MarkerExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        savepoint.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {MARKER_TABLE} (value INTEGER NOT NULL)"
        ))?;
        savepoint.execute(&format!("INSERT INTO {MARKER_TABLE}(value) VALUES (1)"), [])?;
        if let Some(barrier) = &self.control.after_mutation {
            barrier.arrive_and_wait();
        }
        if self.control.panic_after_mutation {
            panic!("injected actor executor panic");
        }
        if let Some(entered) = &self.control.entered {
            let _ = entered.send(());
        }
        if let Some(release) = &self.control.release {
            let (released, condition) = &**release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(crate) struct LifecycleBarrier {
    state: Arc<(Mutex<(bool, bool)>, Condvar)>,
}

impl LifecycleBarrier {
    pub(crate) fn wait_until_arrived(&self) {
        let (state, condition) = &*self.state;
        let state = state.lock().unwrap();
        let (state, timeout) = condition
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.0)
            .unwrap();
        assert!(state.0 && !timeout.timed_out(), "lifecycle event timed out");
    }

    pub(crate) fn arrive_and_wait(&self) {
        let (state, condition) = &*self.state;
        let mut state = state.lock().unwrap();
        state.0 = true;
        condition.notify_all();
        let (state, timeout) = condition
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.1)
            .unwrap();
        assert!(
            state.1 && !timeout.timed_out(),
            "lifecycle release timed out"
        );
    }

    pub(crate) fn release(&self) {
        let (state, condition) = &*self.state;
        state.lock().unwrap().1 = true;
        condition.notify_all();
    }
}

pub(crate) struct TestProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<AtomicU8>,
    commit_started: AtomicBool,
    after_commit: Option<(std::path::PathBuf, i64, LifecycleBarrier)>,
}

impl TestProbe {
    pub(crate) fn fixed(request: &RuntimeSubmitRequestV1) -> Arc<Self> {
        Self::controlled(request, Arc::new(AtomicU8::new(0)))
    }

    pub(crate) fn controlled(
        request: &RuntimeSubmitRequestV1,
        interruption: Arc<AtomicU8>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption,
            commit_started: AtomicBool::new(false),
            after_commit: None,
        })
    }

    pub(crate) fn pause_after_commit(
        request: &RuntimeSubmitRequestV1,
        interruption: Arc<AtomicU8>,
        database: &TestDatabase,
        barrier: LifecycleBarrier,
    ) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption,
            commit_started: AtomicBool::new(false),
            after_commit: Some((database.path.clone(), marker_count(database), barrier)),
        })
    }
}

impl RuntimeRequestProbeV1 for TestProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        if let Some((path, baseline, barrier)) = &self.after_commit {
            let committed_count = Connection::open(path)
                .ok()
                .and_then(|connection| {
                    let exists = connection
                        .query_row(
                            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                            [MARKER_TABLE],
                            |row| row.get::<_, i64>(0),
                        )
                        .ok()?;
                    (exists == 1)
                        .then(|| {
                            connection
                                .query_row(
                                    &format!("SELECT COUNT(*) FROM {MARKER_TABLE}"),
                                    [],
                                    |row| row.get::<_, i64>(0),
                                )
                                .ok()
                        })
                        .flatten()
                })
                .unwrap_or(0);
            if committed_count > *baseline {
                barrier.arrive_and_wait();
            }
        }
        match self.interruption.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(RuntimeInterruptionV1::Cancelled),
            _ => Some(RuntimeInterruptionV1::DeadlineExceeded),
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

pub(crate) fn writer(
    database: &TestDatabase,
    request: &RuntimeSubmitRequestV1,
    config: AdmissionConfigV1,
    control: ExecutorControl,
) -> PersistentWriter {
    let binding = request.binding().clone();
    let locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        LocatorDigest::new(digest('d')).unwrap(),
    );
    PersistentWriter::start(
        ExistingWriterLocator::new(binding, locator, database.path.clone()).unwrap(),
        config,
        MarkerExecutor { control },
    )
    .unwrap()
}

pub(crate) fn unwrap_arc<T>(value: Arc<T>) -> T {
    match Arc::try_unwrap(value) {
        Ok(value) => value,
        Err(_) => panic!("submit tasks retained writer references"),
    }
}

pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

pub(crate) fn marker_count(database: &TestDatabase) -> i64 {
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

pub(crate) fn table_count(database: &TestDatabase, table: &str) -> i64 {
    database
        .connect()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

pub(crate) fn release(control: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, condition) = &**control;
    *released.lock().unwrap() = true;
    condition.notify_all();
}
