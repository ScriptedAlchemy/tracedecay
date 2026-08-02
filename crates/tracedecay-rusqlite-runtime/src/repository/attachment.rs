use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use rusqlite::Transaction;
use tracedecay_store::{
    AdmissionConfigV1, ConsistencyModeV1, FrozenWatermarkCoverageV1, FrozenWatermarkVectorV1,
    RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    ShardWatermarkV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use crate::{
    CheckpointOutcome, CheckpointRequest, ExistingWriterLocator, OnlineBackupReceipt,
    PersistentWriter, RuntimeWriteAuthority, WriterStartError, WriterState,
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    migration_sql::{MigrationSqlError, MigrationSqlHandle},
    reader::{
        ExistingReaderLocator, ReaderAcquireError, ReaderPool, ReaderQueryExecutor,
        ReaderStartError,
    },
};

use super::{ConcreteRepositoryReadExecutor, ConcreteRepositoryWriteExecutor};

const ATTACHMENT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACHMENT_DRAIN_POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Default)]
pub struct RepositoryPhysicalAttachmentFactory;

impl RepositoryPhysicalAttachmentFactory {
    pub fn attach(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        self.attach_with_start_hook(binding, locator, path, admission, &mut |_| {})
    }

    fn attach_with_start_hook(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
        start_hook: &mut dyn FnMut(AttachmentWorkerStartStage),
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        if matches!(binding.shard_id.scope, StoreShardScopeV1::Code { .. }) {
            return Err(RepositoryAttachmentStartError::UnsupportedShardScope);
        }
        let opened_database =
            OpenedDatabaseFile::pin(&path).map_err(RepositoryAttachmentStartError::Identity)?;
        self.attach_opened(
            binding,
            locator,
            path,
            admission,
            opened_database,
            false,
            start_hook,
        )
    }

    pub fn initialize(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        if matches!(binding.shard_id.scope, StoreShardScopeV1::Code { .. }) {
            return Err(RepositoryAttachmentStartError::UnsupportedShardScope);
        }
        let opened_database = OpenedDatabaseFile::create_new(&path)
            .map_err(RepositoryAttachmentStartError::Identity)?;
        self.attach_opened(
            binding,
            locator,
            path,
            admission,
            opened_database,
            true,
            &mut |_| {},
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_opened(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
        opened_database: OpenedDatabaseFile,
        created: bool,
        start_hook: &mut dyn FnMut(AttachmentWorkerStartStage),
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        let writer_locator =
            match ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()) {
                Ok(locator) => {
                    let opened = match opened_database.try_clone() {
                        Ok(opened) => opened,
                        Err(error) => {
                            return Err(repository_start_failure(
                                opened_database,
                                &path,
                                created,
                                RepositoryAttachmentStartError::Identity(error),
                            ));
                        }
                    };
                    locator.with_opened_database(opened)
                }
                Err(error) => {
                    return Err(repository_start_failure(
                        opened_database,
                        &path,
                        created,
                        RepositoryAttachmentStartError::Writer(error),
                    ));
                }
            };
        let reader_locator =
            match ExistingReaderLocator::new(binding.clone(), locator, path.clone()) {
                Ok(locator) => {
                    let opened = match opened_database.try_clone() {
                        Ok(opened) => opened,
                        Err(error) => {
                            return Err(repository_start_failure(
                                opened_database,
                                &path,
                                created,
                                RepositoryAttachmentStartError::Identity(error),
                            ));
                        }
                    };
                    locator.with_opened_database(opened)
                }
                Err(error) => {
                    return Err(repository_start_failure(
                        opened_database,
                        &path,
                        created,
                        RepositoryAttachmentStartError::Reader(error),
                    ));
                }
            };
        start_hook(AttachmentWorkerStartStage::BeforeWriter);
        let writer_result = PersistentWriter::start(
            writer_locator,
            admission.clone(),
            ConcreteRepositoryWriteExecutor::default(),
        );
        start_hook(AttachmentWorkerStartStage::AfterWriter);
        let writer = match writer_result {
            Ok(writer) => writer,
            Err(error) => {
                return Err(repository_start_failure(
                    opened_database,
                    &path,
                    created,
                    RepositoryAttachmentStartError::Writer(error),
                ));
            }
        };
        start_hook(AttachmentWorkerStartStage::BeforeReaders);
        let readers_result = ReaderPool::start_with_checkpoint_pressure(
            reader_locator,
            admission.readers,
            RepositoryRuntimeReadExecutor::default(),
            Some(writer.checkpoint_handle().pressure_subscription()),
        );
        start_hook(AttachmentWorkerStartStage::AfterReaders);
        let readers = match readers_result {
            Ok(readers) => readers,
            Err(error) => {
                let _ = writer.shutdown_and_join();
                return Err(repository_start_failure(
                    opened_database,
                    &path,
                    created,
                    RepositoryAttachmentStartError::Reader(error),
                ));
            }
        };
        let expected_identity = opened_database.identity();
        if writer.opened_file_identity() != Some(expected_identity)
            || readers.opened_file_identity() != Some(expected_identity)
        {
            drop(readers);
            let _ = writer.shutdown_and_join();
            return Err(repository_start_failure(
                opened_database,
                &path,
                created,
                RepositoryAttachmentStartError::Identity(OpenedDatabaseFileError::Replaced),
            ));
        }
        if let Err(error) = opened_database.verify_current_path(&path) {
            drop(readers);
            let _ = writer.shutdown_and_join();
            return Err(repository_start_failure(
                opened_database,
                &path,
                created,
                RepositoryAttachmentStartError::Identity(error),
            ));
        }
        let opened_file_identity = opened_database.identity();
        let initialization_file = created.then_some(opened_database);
        Ok(RepositoryRuntimePhysicalAttachment {
            state: Mutex::new(RepositoryRuntimePhysicalState {
                binding,
                database_path: path,
                opened_file_identity,
                initialization_file,
                writer: Some(Arc::new(writer)),
                readers: Some(readers),
                admission_open: true,
                drained: false,
                closed: false,
                close_failure: None,
            }),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttachmentWorkerStartStage {
    BeforeWriter,
    AfterWriter,
    BeforeReaders,
    AfterReaders,
}

fn repository_start_failure(
    opened_database: OpenedDatabaseFile,
    database_path: &std::path::Path,
    created: bool,
    failure: RepositoryAttachmentStartError,
) -> RepositoryAttachmentStartError {
    if created && let Err(error) = opened_database.discard_created(database_path) {
        return RepositoryAttachmentStartError::Identity(error);
    }
    failure
}

#[derive(Debug)]
pub enum RepositoryAttachmentStartError {
    UnsupportedShardScope,
    Identity(crate::connection::OpenedDatabaseFileError),
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for RepositoryAttachmentStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedShardScope => {
                formatter.write_str("repository attachment does not own code shards")
            }
            Self::Identity(error) => write!(formatter, "identify repository attachment: {error}"),
            Self::Reader(error) => write!(formatter, "start repository readers: {error}"),
            Self::Writer(error) => write!(formatter, "start repository writer: {error}"),
        }
    }
}

impl Error for RepositoryAttachmentStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Identity(error) => Some(error),
            Self::Reader(error) => Some(error),
            Self::Writer(error) => Some(error),
            Self::UnsupportedShardScope => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepositoryRuntimePhysicalSnapshot {
    pub healthy: bool,
    pub writer_present: bool,
    pub reader_handles: u32,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub wal_bytes: u64,
}

impl RepositoryRuntimePhysicalSnapshot {
    pub const fn is_drained(self) -> bool {
        !self.writer_present
            && self.reader_handles == 0
            && self.queued_operations == 0
            && self.queued_bytes == 0
    }
}

pub struct RepositoryRuntimePhysicalAttachment {
    state: Mutex<RepositoryRuntimePhysicalState>,
}

struct RepositoryRuntimePhysicalState {
    binding: StoreRuntimeBindingV1,
    database_path: PathBuf,
    opened_file_identity: u64,
    initialization_file: Option<OpenedDatabaseFile>,
    writer: Option<Arc<PersistentWriter>>,
    readers: Option<ReaderPool<RepositoryRuntimeReadExecutor>>,
    admission_open: bool,
    drained: bool,
    closed: bool,
    close_failure: Option<String>,
}

impl RepositoryRuntimePhysicalAttachment {
    pub fn binding(&self) -> StoreRuntimeBindingV1 {
        self.lock_state().binding.clone()
    }

    pub fn opened_file_identity(&self) -> u64 {
        self.lock_state().opened_file_identity
    }

    pub fn commit_initialization(&self) -> Result<(), String> {
        let mut state = self.lock_state();
        let opened = state
            .initialization_file
            .as_ref()
            .ok_or_else(|| "repository attachment has no pending initialization".to_owned())?;
        opened
            .verify_current_path(&state.database_path)
            .map_err(|error| error.to_string())?;
        state.initialization_file.take();
        Ok(())
    }

    pub fn abort_initialization(&self) -> Result<(), String> {
        self.drain()?;
        self.close_and_join()?;
        let mut state = self.lock_state();
        let Some(opened) = state.initialization_file.take() else {
            return Ok(());
        };
        opened
            .discard_created(&state.database_path)
            .map_err(|error| error.to_string())
    }

    pub fn migration_sql_handle(&self) -> Result<MigrationSqlHandle, MigrationSqlError> {
        let state = self.lock_state();
        if !state.admission_open || state.closed {
            return Err(MigrationSqlError::WriterUnavailable);
        }
        let writer = state
            .writer
            .as_deref()
            .ok_or(MigrationSqlError::WriterUnavailable)?;
        let readers = state.readers.as_ref().ok_or_else(|| {
            MigrationSqlError::ReaderUnavailable("repository readers are unavailable".to_owned())
        })?;
        MigrationSqlHandle::attach(writer, readers)
    }

    pub fn snapshot(&self) -> RepositoryRuntimePhysicalSnapshot {
        let state = self.lock_state();
        let writer = state.writer.as_ref();
        let writer_telemetry = writer.map(|writer| writer.telemetry_snapshot());
        let readers = state.readers.as_ref().map(ReaderPool::snapshot);
        let reader_handles = readers.map_or(0, |snapshot| {
            u32::from(snapshot.general_workers) + u32::from(snapshot.health_workers)
        });
        RepositoryRuntimePhysicalSnapshot {
            healthy: writer.is_none_or(|writer| writer.state() != WriterState::Faulted),
            writer_present: writer.is_some(),
            reader_handles,
            queued_operations: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_operations),
            queued_bytes: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_bytes),
            wal_bytes: wal_bytes(&state.database_path),
        }
    }

    pub async fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<RuntimeSubmitOutcomeV1, RepositoryDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .writer
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        writer
            .submit_authorized(request, probe, authority)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u32,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<(), RepositoryDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .writer
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        writer
            .bounded_incremental_vacuum(max_pages, authority)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub async fn run_checkpoint(
        &self,
        request: CheckpointRequest,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<CheckpointOutcome, RepositoryDispatchError> {
        let checkpoint = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .writer
                .as_ref()
                .ok_or(RepositoryDispatchError::Closed)?
                .checkpoint_handle()
        };
        let ticket = checkpoint
            .trigger_authorized(request, authority)
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))?;
        ticket
            .wait()
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub async fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<OnlineBackupReceipt, RepositoryDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .writer
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        writer
            .snapshot_to(destination, authority)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, RepositoryDispatchError> {
        let readers = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .readers
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        let mut reader = readers
            .acquire_for_dispatch(&request, probe)
            .map_err(RepositoryDispatchError::Reader)?;
        let mut snapshot = reader
            .begin_snapshot()
            .map_err(|error| RepositoryDispatchError::ReaderWorker(error.to_string()))?;
        snapshot
            .execute(request, probe)
            .map_err(RepositoryDispatchError::Reader)
    }

    pub fn drain(&self) -> Result<(), String> {
        let mut state = self.lock_state();
        if state.closed {
            return Ok(());
        }
        if state.drained {
            return Ok(());
        }
        state.admission_open = false;
        if let Some(writer) = &state.writer {
            writer.begin_drain();
        }
        if let Some(readers) = &state.readers {
            readers.begin_shutdown_drain();
        }
        drop(state);

        let started = Instant::now();
        loop {
            let state = self.lock_state();
            let writer_quiescent = state.writer.as_ref().is_none_or(|writer| {
                Arc::strong_count(writer) == 1
                    && writer.telemetry_snapshot().queue.queued_operations == 0
            });
            let readers_quiescent = state.readers.as_ref().is_none_or(ReaderPool::is_quiescent);
            if writer_quiescent && readers_quiescent {
                break;
            }
            if started.elapsed() >= ATTACHMENT_DRAIN_TIMEOUT {
                let queued = state
                    .writer
                    .as_ref()
                    .map(|writer| writer.telemetry_snapshot().queue.queued_operations)
                    .unwrap_or(0);
                let leased = state.readers.as_ref().map_or(0, |readers| {
                    let snapshot = readers.snapshot();
                    u32::from(snapshot.leased_general) + u32::from(snapshot.leased_health)
                });
                return Err(format!(
                    "repository physical attachment did not quiesce within {ATTACHMENT_DRAIN_TIMEOUT:?}: {leased} leased readers and {queued} queued writes"
                ));
            }
            drop(state);
            thread::sleep(ATTACHMENT_DRAIN_POLL);
        }

        let mut state = self.lock_state();
        let writer = match state.writer.take().map(Arc::try_unwrap).transpose() {
            Ok(writer) => writer,
            Err(writer) => {
                state.writer = Some(writer);
                return Err("repository writer is still serving a request".to_owned());
            }
        };
        let readers = state.readers.take();
        drop(readers);
        if let Some(writer) = writer
            && let Err(error) = writer.shutdown_and_join()
        {
            let message = format!("join repository writer: {error}");
            state.close_failure = Some(message.clone());
            return Err(message);
        }
        state.drained = true;
        Ok(())
    }

    pub fn close_and_join(&self) -> Result<(), String> {
        let mut state = self.lock_state();
        if state.closed {
            return match &state.close_failure {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            };
        }
        if state.admission_open {
            return Err("repository physical attachment must drain before close".to_owned());
        }
        if !state.drained {
            return Err("repository physical attachment has not completed drain".to_owned());
        }
        if state.writer.is_some() || state.readers.is_some() {
            return Err("repository physical attachment retained handles after drain".to_owned());
        }
        state.closed = true;
        Ok(())
    }

    fn lock_state(&self) -> MutexGuard<'_, RepositoryRuntimePhysicalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for RepositoryRuntimePhysicalAttachment {
    fn drop(&mut self) {
        let pending = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .initialization_file
            .is_some();
        if pending {
            let _ = self.abort_initialization();
        }
    }
}

#[derive(Debug)]
pub enum RepositoryDispatchError {
    Closed,
    Reader(ReaderAcquireError),
    ReaderWorker(String),
    Writer(String),
}

impl fmt::Display for RepositoryDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("repository runtime is closed"),
            Self::Reader(error) => write!(formatter, "repository read failed: {error}"),
            Self::ReaderWorker(error) => write!(formatter, "repository snapshot failed: {error}"),
            Self::Writer(error) => write!(formatter, "repository write failed: {error}"),
        }
    }
}

impl Error for RepositoryDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reader(error) => Some(error),
            Self::Closed | Self::ReaderWorker(_) | Self::Writer(_) => None,
        }
    }
}

#[derive(Clone, Default)]
struct RepositoryRuntimeReadExecutor {
    repository: ConcreteRepositoryReadExecutor,
}

impl ReaderQueryExecutor for RepositoryRuntimeReadExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let value = match request.operation() {
            RuntimeReadOperationV1::TemporalHealth => {
                let healthy = snapshot
                    .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                    .map(|value| value.eq_ignore_ascii_case("ok"))
                    .map_err(|error| infrastructure(format!("repository quick check: {error}")))?;
                RuntimeReadResultV1::TemporalHealth { healthy }
            }
            RuntimeReadOperationV1::Repository { op } => {
                let result = self
                    .repository
                    .execute(snapshot, op)
                    .map_err(|error| infrastructure(format!("repository read: {error}")))?;
                RuntimeReadResultV1::Repository { result }
            }
            _ => {
                return Err(infrastructure(
                    "repository reader received an unsupported runtime operation",
                ));
            }
        };
        let coverage = match request.consistency() {
            ConsistencyModeV1::LatestAvailable => RuntimeReadCoverageV1::Latest { observed: None },
            ConsistencyModeV1::AtLeast { commit_sequence } => {
                let observed = ShardWatermarkV1 {
                    shard_id: request.binding().shard_id.clone(),
                    incarnation: request.binding().incarnation,
                    authority_epoch: request.binding().authority_epoch,
                    commit_sequence: *commit_sequence,
                };
                let required =
                    FrozenWatermarkVectorV1::new([observed.clone()]).map_err(|error| {
                        infrastructure(format!("construct repository required watermark: {error}"))
                    })?;
                let coverage =
                    FrozenWatermarkCoverageV1::new(required, [observed]).map_err(|error| {
                        infrastructure(format!("construct repository read coverage: {error}"))
                    })?;
                RuntimeReadCoverageV1::Complete { coverage }
            }
            ConsistencyModeV1::ExactSnapshot { .. }
            | ConsistencyModeV1::FrozenWatermarkVector { .. } => {
                return Err(infrastructure(
                    "repository reader does not support snapshot consistency",
                ));
            }
        };
        RuntimeReadOutcomeV1::new(Some(value), coverage)
            .map_err(|error| infrastructure(format!("construct repository read outcome: {error}")))
    }
}

fn infrastructure(operation: impl Into<String>) -> StorageRuntimeErrorV1 {
    StorageRuntimeErrorV1::Infrastructure {
        operation: operation.into(),
    }
}

fn wal_bytes(database_path: &std::path::Path) -> u64 {
    let mut name = database_path.as_os_str().to_os_string();
    name.push("-wal");
    std::fs::metadata(PathBuf::from(name)).map_or(0, |metadata| metadata.len())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{AdmissionConfigV1, StoreIncarnationV1};

    use crate::migration_sql::{MigrationSqlError, MigrationSqlStatement, MigrationSqlValue};

    use super::*;

    fn binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.repository-lifecycle",
                "profile_id": "profile.repository-lifecycle",
                "scope": {
                    "kind": "project",
                    "project_id": "project.repository-lifecycle"
                }
            },
            "incarnation": 4,
            "authority_epoch": 12
        }))
        .unwrap()
    }

    fn locator(binding: &StoreRuntimeBindingV1) -> VerifiedStoreLocatorV1 {
        VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(4).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        )
    }

    fn statement(sql: &str, params: Vec<MigrationSqlValue>) -> MigrationSqlStatement {
        MigrationSqlStatement::new(sql.to_owned(), params).unwrap()
    }

    fn create_identity_database(path: &std::path::Path, value: &str) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch("CREATE TABLE identity_probe (value TEXT NOT NULL)")
            .unwrap();
        connection
            .execute("INSERT INTO identity_probe (value) VALUES (?)", [value])
            .unwrap();
    }

    #[test]
    fn writer_binds_pinned_file_across_a_b_a_path_swap() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("repository.sqlite3");
        let displaced = directory.path().join("repository-a.sqlite3");
        let replacement = directory.path().join("repository-b.sqlite3");
        create_identity_database(&path, "A");
        create_identity_database(&replacement, "B");
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let result = RepositoryPhysicalAttachmentFactory.attach_with_start_hook(
            binding.clone(),
            locator(&binding),
            path.clone(),
            AdmissionConfigV1::default(),
            &mut |stage| match stage {
                AttachmentWorkerStartStage::BeforeWriter => {
                    fs::rename(&path, &displaced).unwrap();
                    fs::rename(&replacement, &path).unwrap();
                }
                AttachmentWorkerStartStage::AfterWriter => {
                    fs::rename(&path, &replacement).unwrap();
                    fs::rename(&displaced, &path).unwrap();
                }
                _ => {}
            },
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("attachment unexpectedly started across path replacement"),
        };
        assert!(matches!(error, RepositoryAttachmentStartError::Writer(_)));

        let canonical_value: String = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT value FROM identity_probe", [], |row| row.get(0))
            .unwrap();
        let replacement_value: String = rusqlite::Connection::open(&replacement)
            .unwrap()
            .query_row("SELECT value FROM identity_probe", [], |row| row.get(0))
            .unwrap();
        assert_eq!(canonical_value, "A");
        assert_eq!(replacement_value, "B");
    }

    #[test]
    fn real_sqlite_attachment_reopens_and_rejects_stale_handles_after_exact_once_close() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("repository.sqlite3");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let locator = locator(&binding);
        let factory = RepositoryPhysicalAttachmentFactory;

        for cycle in 0_i64..3 {
            let attachment = factory
                .attach(
                    binding.clone(),
                    locator.clone(),
                    path.clone(),
                    AdmissionConfigV1::default(),
                )
                .unwrap();
            let handle = attachment.migration_sql_handle().unwrap();
            handle
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS runtime_lifecycle (
                        cycle INTEGER PRIMARY KEY
                    )"
                    .to_owned(),
                )
                .unwrap();
            handle
                .execute(statement(
                    "INSERT INTO runtime_lifecycle (cycle) VALUES (?)",
                    vec![MigrationSqlValue::Integer(cycle)],
                ))
                .unwrap();
            let rows = handle
                .query(
                    statement("SELECT cycle FROM runtime_lifecycle ORDER BY cycle", vec![]),
                    Duration::from_secs(1),
                )
                .unwrap();
            assert_eq!(rows.rows.len(), usize::try_from(cycle + 1).unwrap());
            assert_eq!(
                rows.rows.last().unwrap().values,
                vec![MigrationSqlValue::Integer(cycle)]
            );

            attachment.drain().unwrap();
            attachment.close_and_join().unwrap();
            {
                let state = attachment.lock_state();
                assert!(state.closed);
                assert!(state.writer.is_none());
                assert!(state.readers.is_none());
            }
            attachment.close_and_join().unwrap();

            let write_error = handle
                .execute(statement(
                    "INSERT INTO runtime_lifecycle (cycle) VALUES (?)",
                    vec![MigrationSqlValue::Integer(cycle + 10)],
                ))
                .unwrap_err();
            assert_eq!(write_error, MigrationSqlError::WriterUnavailable);
            let read_error = handle
                .query(
                    statement("SELECT cycle FROM runtime_lifecycle", vec![]),
                    Duration::ZERO,
                )
                .unwrap_err();
            assert!(matches!(
                read_error,
                MigrationSqlError::ReaderUnavailable(_)
            ));
        }
    }
}
