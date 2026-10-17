//! Synchronous registered workflow store for the storage suites.

use std::path::PathBuf;

use rusqlite::{Connection, Savepoint};
use tempfile::TempDir;
use tracedecay_contracts::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffGrant, TaskHandoffScope, WorkflowActiveRunRecoveryCursorV1,
    WorkflowActiveRunRecoveryPageV1, WorkflowArtifactPayload, WorkflowArtifactPersistOutcome,
    WorkflowArtifactStoreError, WorkflowArtifactStorePort, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowDefinitionDisposition,
    WorkflowDefinitionLifecycleCommand, WorkflowDefinitionTransitionOutcome,
    WorkflowEffectAuthorityErrorV1, WorkflowEffectAuthorityPortV1, WorkflowEffectIdentityV1,
    WorkflowEffectJournalRecordV1, WorkflowEffectPreparedV1, WorkflowFanOutAttemptBindingV1,
    WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError,
    WorkflowFanOutCensusObservationV1, WorkflowFanOutCensusPersistOutcomeV1,
    WorkflowFanOutCensusReadPort, WorkflowFanOutCensusStoragePort, WorkflowRunAppendOutcome,
    WorkflowRunAppendRequest, WorkflowRunReadPort, WorkflowRunStorageError, WorkflowRunStoragePort,
};
use tracedecay_domain::{
    LocatorDigest, ManifestDigest, RunId, UtcMicros, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAuthority, WorkflowDefinition, WorkflowDefinitionId, WorkflowFanOutCensusV1,
    WorkflowRunProjection,
};
use tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle;
use tracedecay_rusqlite_runtime::native_runtime::block_on;
use tracedecay_rusqlite_runtime::reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor};
use tracedecay_rusqlite_runtime::repository::RetainedExactSqlCapability;
use tracedecay_rusqlite_runtime::work_run_control::WorkSqliteJobStorage;
use tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority;
use tracedecay_rusqlite_runtime::workflow::install_workflow_schema;
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
};
use tracedecay_store::{
    AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    StorageRuntimeErrorV1, StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

struct NoTypedWrites;

struct WorkflowStoreTestRetentionGuard;

impl StorageOperationExecutor for NoTypedWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        unreachable!("workflow writes only through the registered exact-SQL channel")
    }
}

#[derive(Clone)]
struct NoTypedReads;

impl ReaderQueryExecutor for NoTypedReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("workflow reads only through the registered exact-SQL channel")
    }
}

/// A started registered workflow store.
pub struct RegisteredWorkflowStore {
    storage: ExactSqlHandle,
    path: PathBuf,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoTypedReads>,
    _directory: TempDir,
}

impl RegisteredWorkflowStore {
    pub fn start(name: &str) -> Self {
        Self::start_with_setup(name, |_| {})
    }

    /// Starts a registered store, running `setup` against the file after the
    /// workflow schema is installed and before the writer takes ownership.
    pub fn start_with_setup(name: &str, setup: impl FnOnce(&Connection)) -> Self {
        let directory = TempDir::new().expect("workflow store directory");
        let path = directory.path().join(format!("{name}.sqlite3"));
        {
            let connection = Connection::open(&path).expect("open workflow store");
            install_workflow_schema(&connection).expect("install workflow schema");
            connection
                .execute_batch(tracedecay_rusqlite_runtime::handoff::HANDOFF_OPEN_SCHEMA_V1)
                .expect("install handoff-open schema");
            setup(&connection);
        }
        let path = path.canonicalize().expect("canonicalize workflow store");
        Self::open(name, path, directory)
    }

    /// Stops this store and starts a new one over the same file, the way a
    /// daemon restart rebinds the registered channel to persisted state.
    pub fn restart(self, name: &str) -> Self {
        let Self {
            storage,
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        } = self;
        drop(storage);
        drop(readers);
        drop(writer);
        Self::open(name, path, directory)
    }

    fn open(name: &str, path: PathBuf, directory: TempDir) -> Self {
        let binding = binding(name);
        let locator = locator(&binding);
        let writer = PersistentWriter::start(
            &tracedecay_rusqlite_runtime::native_runtime::handle().expect("native runtime"),
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
                .expect("workflow store writer locator"),
            AdmissionConfigV1::default(),
            NoTypedWrites,
        )
        .expect("start workflow store writer");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding, locator, path.clone())
                .expect("workflow store reader locator"),
            AdmissionConfigV1::default().readers,
            NoTypedReads,
        )
        .expect("start workflow store readers");
        let handle = ExactSqlHandle::attach(&writer, &readers).expect("attach workflow store");
        Self {
            storage: handle,
            path,
            _writer: writer,
            _readers: readers,
            _directory: directory,
        }
    }

    pub fn retained_exact_sql(&self) -> RetainedExactSqlCapability {
        RetainedExactSqlCapability::from_authorized_handle_with_guard(
            self.storage.clone(),
            WorkflowStoreTestRetentionGuard,
        )
    }

    /// Opens a short-lived connection for assertions that inspect stored rows
    /// directly. Writes still go through the registered channel.
    pub fn inspect<T>(&self, read: impl FnOnce(&Connection) -> T) -> T {
        let connection = Connection::open(&self.path).expect("open workflow store for inspection");
        read(&connection)
    }

    pub fn count(&self, table: &str) -> i64 {
        self.inspect(|connection| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap_or_else(|error| panic!("count {table}: {error}"))
        })
    }
}

fn binding(name: &str) -> StoreRuntimeBindingV1 {
    serde_json::from_value(serde_json::json!({
        "shard_id": {
            "brain_id": "brain.work-storage",
            "profile_id": "profile.work-storage",
            "scope": { "kind": "project", "project_id": format!("project.work-storage.{name}") }
        },
        "incarnation": 1,
        "authority_epoch": 1
    }))
    .expect("workflow store binding")
}

fn locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
    VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        StoreIncarnationV1::new(1).expect("workflow store incarnation"),
        LocatorDigest::new(format!("sha256:{}", "5".repeat(64))).expect("workflow store digest"),
    )
}

#[derive(Clone)]
pub struct WorkflowJobStorage(pub WorkflowSqliteAuthority);

impl WorkflowJobStorage {
    pub fn attach(store: &RegisteredWorkflowStore) -> Self {
        Self(
            block_on(WorkflowSqliteAuthority::from_retained_exact_sql(
                store.retained_exact_sql(),
            ))
            .expect("native runtime")
            .expect("attach workflow authority"),
        )
    }

    fn job<R: Send + 'static, E: Send + 'static>(
        &self,
        job: impl Fn(WorkSqliteJobStorage<'_>) -> Result<R, E> + Send + Sync + 'static,
        unavailable: E,
    ) -> Result<R, E> {
        block_on(self.0.transact(job))
            .expect("native runtime")
            .unwrap_or(Err(unavailable))
    }
}

impl WorkflowRunReadPort for WorkflowJobStorage {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        self.0.projection(run_id)
    }

    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
        self.0.projections()
    }

    fn active_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
        self.0.active_projection_page(authority, after)
    }

    fn fan_out_binding(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
        self.0.fan_out_binding(identity)
    }
}

impl WorkflowRunStoragePort for WorkflowJobStorage {
    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
        let request = request.clone();
        self.job(
            move |storage| storage.append(&request),
            WorkflowRunStorageError::Unavailable,
        )
    }
}

impl WorkflowArtifactStorePort for WorkflowJobStorage {
    fn persist(
        &self,
        payload: &WorkflowArtifactPayload,
    ) -> Result<WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError> {
        let payload = payload.clone();
        self.job(
            move |storage| storage.persist(&payload),
            WorkflowArtifactStoreError::Unavailable,
        )
    }

    fn load(
        &self,
        artifact: &WorkArtifactRefV1,
    ) -> Result<WorkflowArtifactPayload, WorkflowArtifactStoreError> {
        let artifact = artifact.clone();
        self.job(
            move |storage| storage.load(&artifact),
            WorkflowArtifactStoreError::Unavailable,
        )
    }
}

impl WorkflowFanOutCensusReadPort for WorkflowJobStorage {
    fn latest_census(
        &self,
        run_id: &RunId,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
        self.0.latest_census(run_id)
    }

    fn census_before(
        &self,
        run_id: &RunId,
        workflow_sequence: u64,
    ) -> Result<Option<WorkflowFanOutCensusV1>, WorkflowFanOutCensusError> {
        self.0.census_before(run_id, workflow_sequence)
    }

    fn pending_census_observations(
        &self,
        limit: u16,
    ) -> Result<Vec<WorkflowFanOutCensusObservationV1>, WorkflowFanOutCensusError> {
        self.0.pending_census_observations(limit)
    }

    fn census_backfill_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowFanOutCensusBackfillPageV1, WorkflowFanOutCensusError> {
        self.0.census_backfill_projection_page(authority, after)
    }
}

impl WorkflowFanOutCensusStoragePort for WorkflowJobStorage {
    fn persist_census(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<WorkflowFanOutCensusPersistOutcomeV1, WorkflowFanOutCensusError> {
        let census = census.clone();
        self.job(
            move |storage| storage.persist_census(&census),
            WorkflowFanOutCensusError::Unavailable,
        )
    }

    fn mark_census_observability_durable(
        &self,
        census: &WorkflowFanOutCensusV1,
    ) -> Result<(), WorkflowFanOutCensusError> {
        let census = census.clone();
        self.job(
            move |storage| storage.mark_census_observability_durable(&census),
            WorkflowFanOutCensusError::Unavailable,
        )
    }
}

impl WorkflowEffectAuthorityPortV1 for WorkflowJobStorage {
    fn has_pending_effects(
        &self,
        worktree_id: &tracedecay_domain::WorktreeId,
    ) -> Result<bool, WorkflowEffectAuthorityErrorV1> {
        self.0.has_pending_effects(worktree_id)
    }

    fn reserve_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
        prepared: &WorkflowEffectPreparedV1,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
        let (identity, prepared) = (identity.clone(), prepared.clone());
        self.job(
            move |storage| storage.reserve_effect(&identity, &prepared),
            WorkflowEffectAuthorityErrorV1::Unavailable("writer job".to_owned()),
        )
    }

    fn execute_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
        prepared: &WorkflowEffectPreparedV1,
        ended_at: UtcMicros,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1> {
        let (identity, prepared) = (identity.clone(), prepared.clone());
        self.job(
            move |storage| storage.execute_effect(&identity, &prepared, ended_at),
            WorkflowEffectAuthorityErrorV1::Unavailable("writer job".to_owned()),
        )
    }
}

impl WorkflowDefinitionAuthorityPort for WorkflowJobStorage {
    fn insert(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        self.0.insert(definition)
    }

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        self.0.load(definition_id, definition_version)
    }

    fn list(
        &self,
        definition_id: Option<&WorkflowDefinitionId>,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        self.0.list(definition_id)
    }

    fn load_disposition(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionDisposition>, WorkflowDefinitionAuthorityError> {
        self.0.load_disposition(definition_id, definition_version)
    }

    fn transition(
        &self,
        command: &WorkflowDefinitionLifecycleCommand,
    ) -> Result<WorkflowDefinitionTransitionOutcome, WorkflowDefinitionAuthorityError> {
        self.0.transition(command)
    }
}

impl TaskHandoffAuthorityPort for WorkflowJobStorage {
    fn issue(&self, grant: &TaskHandoffGrant) -> Result<(), TaskHandoffAuthorityError> {
        self.0.issue(grant)
    }

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected_scope: &TaskHandoffScope,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError> {
        self.0.consume(token_digest, expected_scope, consumed_at)
    }
}
