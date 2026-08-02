use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Savepoint, Transaction};
use tracedecay_store::{
    AdmissionConfigV1, IdempotencyIdentityV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StorageRuntimeErrorV1,
    StoreCommitReceiptV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    CheckpointOutcome, CheckpointRequest, ExistingWriterLocator, OnlineBackupReceipt,
    PersistentWriter, RuntimeWriteAuthority, WriterStartError, WriterState,
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    migration_sql::{MigrationSqlError, MigrationSqlHandle},
    reader::{ExistingReaderLocator, ReaderAcquireError, ReaderPool, ReaderStartError},
    writer::WriterPersistence,
};

use super::{
    CodeShardLocatorError, CodeShardPhysicalLocator, GraphMutationExecutor, GraphReaderExecutor,
};

const ATTACHMENT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACHMENT_DRAIN_POLL: Duration = Duration::from_millis(5);

/// Pre-open physical parts that a later daemon registry adapter can own.
///
/// Preparing parts validates the existing locator and runtime binding but does
/// not start workers, publish a runtime, create a database, or make this path
/// authoritative.
pub struct GraphPhysicalAttachmentParts {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    reader_locator: ExistingReaderLocator,
    writer_locator: Option<ExistingWriterLocator>,
    reader_executor: GraphReaderExecutor,
    mutation_executor: Option<GraphMutationExecutor>,
}

impl GraphPhysicalAttachmentParts {
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    pub fn reader_locator(&self) -> &ExistingReaderLocator {
        &self.reader_locator
    }

    pub fn writer_locator(&self) -> Option<&ExistingWriterLocator> {
        self.writer_locator.as_ref()
    }

    pub const fn reader_executor(&self) -> GraphReaderExecutor {
        self.reader_executor
    }

    pub const fn mutation_executor(&self) -> Option<GraphMutationExecutor> {
        self.mutation_executor
    }

    pub fn into_reader_parts(self) -> (ExistingReaderLocator, GraphReaderExecutor) {
        (self.reader_locator, self.reader_executor)
    }

    pub fn into_writer_parts(self) -> Option<(ExistingWriterLocator, GraphMutationExecutor)> {
        self.writer_locator.zip(self.mutation_executor)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GraphPhysicalAttachmentFactory;

impl GraphPhysicalAttachmentFactory {
    pub fn prepare(
        &self,
        physical: &CodeShardPhysicalLocator,
    ) -> Result<GraphPhysicalAttachmentParts, GraphPhysicalAttachmentPrepareError> {
        let binding = physical.binding().clone();
        let verified = physical.verified().clone();
        let reader_locator = ExistingReaderLocator::new(
            binding.clone(),
            verified.clone(),
            physical.path().to_path_buf(),
        )
        .map_err(GraphPhysicalAttachmentPrepareError::Reader)?;
        let writer_locator = physical
            .is_mutable()
            .then(|| {
                ExistingWriterLocator::new(
                    binding.clone(),
                    verified.clone(),
                    physical.path().to_path_buf(),
                )
            })
            .transpose()
            .map_err(GraphPhysicalAttachmentPrepareError::Writer)?;
        Ok(GraphPhysicalAttachmentParts {
            binding,
            locator: verified,
            reader_locator,
            writer_locator,
            reader_executor: GraphReaderExecutor::new(physical.access()),
            mutation_executor: physical.is_mutable().then_some(GraphMutationExecutor),
        })
    }

    /// Opens real native reader workers and, for mutable worktree shards, one
    /// fenced writer actor. The writer deliberately rejects every repository
    /// payload until the graph mutation DTO is promoted into the store
    /// contract; this prevents the pre-cutover attachment from becoming a
    /// hidden production write path.
    pub fn attach(
        &self,
        physical: &CodeShardPhysicalLocator,
        admission: AdmissionConfigV1,
    ) -> Result<GraphRuntimePhysicalAttachment, GraphPhysicalAttachmentStartError> {
        self.attach_with_start_hook(physical, admission, &mut |_| {})
    }

    fn attach_with_start_hook(
        &self,
        physical: &CodeShardPhysicalLocator,
        admission: AdmissionConfigV1,
        start_hook: &mut dyn FnMut(AttachmentWorkerStartStage),
    ) -> Result<GraphRuntimePhysicalAttachment, GraphPhysicalAttachmentStartError> {
        let database_path = physical.path().to_path_buf();
        let opened_database = OpenedDatabaseFile::pin(&database_path)
            .map_err(GraphPhysicalAttachmentStartError::Identity)?;
        self.attach_opened(physical, admission, opened_database, false, start_hook)
    }

    pub fn initialize(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        database_path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<GraphRuntimePhysicalAttachment, GraphPhysicalAttachmentStartError> {
        let opened_database = OpenedDatabaseFile::create_new(&database_path)
            .map_err(GraphPhysicalAttachmentStartError::Identity)?;
        let physical = match CodeShardPhysicalLocator::from_verified_existing(
            binding,
            locator,
            database_path.clone(),
        ) {
            Ok(physical) if physical.is_mutable() => physical,
            Ok(_) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    true,
                    GraphPhysicalAttachmentStartError::ImmutableInitialization,
                ));
            }
            Err(error) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    true,
                    GraphPhysicalAttachmentStartError::Locator(error),
                ));
            }
        };
        self.attach_opened(&physical, admission, opened_database, true, &mut |_| {})
    }

    fn attach_opened(
        &self,
        physical: &CodeShardPhysicalLocator,
        admission: AdmissionConfigV1,
        opened_database: OpenedDatabaseFile,
        created: bool,
        start_hook: &mut dyn FnMut(AttachmentWorkerStartStage),
    ) -> Result<GraphRuntimePhysicalAttachment, GraphPhysicalAttachmentStartError> {
        let database_path = physical.path().to_path_buf();
        let parts = match self.prepare(physical) {
            Ok(parts) => parts,
            Err(error) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    created,
                    GraphPhysicalAttachmentStartError::Prepare(error),
                ));
            }
        };
        let GraphPhysicalAttachmentParts {
            binding,
            mut reader_locator,
            writer_locator,
            reader_executor,
            ..
        } = parts;
        reader_locator = reader_locator.with_opened_database(match opened_database.try_clone() {
            Ok(opened) => opened,
            Err(error) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    created,
                    GraphPhysicalAttachmentStartError::Identity(error),
                ));
            }
        });
        let writer_locator = match writer_locator
            .map(|locator| {
                opened_database
                    .try_clone()
                    .map(|opened| locator.with_opened_database(opened))
            })
            .transpose()
        {
            Ok(locator) => locator,
            Err(error) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    created,
                    GraphPhysicalAttachmentStartError::Identity(error),
                ));
            }
        };
        let reader_budget = admission.readers.clone();
        start_hook(AttachmentWorkerStartStage::BeforeWriter);
        let writer_result = writer_locator
            .map(|locator| {
                PersistentWriter::start_with_persistence(
                    locator,
                    admission,
                    Box::new(PrecutoverRejectingGraphWriterPersistence),
                )
                .map(Arc::new)
            })
            .transpose();
        start_hook(AttachmentWorkerStartStage::AfterWriter);
        let writer = match writer_result {
            Ok(writer) => writer,
            Err(error) => {
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    created,
                    GraphPhysicalAttachmentStartError::Writer(error),
                ));
            }
        };
        start_hook(AttachmentWorkerStartStage::BeforeReaders);
        let checkpoint_pressure = writer
            .as_ref()
            .map(|writer| writer.checkpoint_handle().pressure_subscription());
        let readers_result = ReaderPool::start_with_checkpoint_pressure(
            reader_locator,
            reader_budget,
            reader_executor,
            checkpoint_pressure,
        );
        start_hook(AttachmentWorkerStartStage::AfterReaders);
        let readers = match readers_result {
            Ok(readers) => readers,
            Err(error) => {
                if let Some(writer) = writer.and_then(|writer| Arc::try_unwrap(writer).ok()) {
                    let _ = writer.shutdown_and_join();
                }
                return Err(graph_start_failure(
                    opened_database,
                    &database_path,
                    created,
                    GraphPhysicalAttachmentStartError::Reader(error),
                ));
            }
        };
        let expected_identity = opened_database.identity();
        if writer
            .as_ref()
            .is_some_and(|writer| writer.opened_file_identity() != Some(expected_identity))
            || readers.opened_file_identity() != Some(expected_identity)
        {
            if let Some(writer) = writer.and_then(|writer| Arc::try_unwrap(writer).ok()) {
                let _ = writer.shutdown_and_join();
            }
            drop(readers);
            return Err(graph_start_failure(
                opened_database,
                &database_path,
                created,
                GraphPhysicalAttachmentStartError::Identity(OpenedDatabaseFileError::Replaced),
            ));
        }
        if let Err(error) = opened_database.verify_current_path(&database_path) {
            if let Some(writer) = writer.and_then(|writer| Arc::try_unwrap(writer).ok()) {
                let _ = writer.shutdown_and_join();
            }
            drop(readers);
            return Err(graph_start_failure(
                opened_database,
                &database_path,
                created,
                GraphPhysicalAttachmentStartError::Identity(error),
            ));
        }
        let opened_file_identity = opened_database.identity();
        let initialization_file = created.then_some(opened_database);
        Ok(GraphRuntimePhysicalAttachment {
            state: Mutex::new(GraphRuntimePhysicalState {
                binding,
                database_path,
                opened_file_identity,
                initialization_file,
                writer,
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

fn graph_start_failure(
    opened_database: OpenedDatabaseFile,
    database_path: &std::path::Path,
    created: bool,
    failure: GraphPhysicalAttachmentStartError,
) -> GraphPhysicalAttachmentStartError {
    if created && let Err(error) = opened_database.discard_created(database_path) {
        return GraphPhysicalAttachmentStartError::Identity(error);
    }
    failure
}

#[derive(Debug)]
pub enum GraphPhysicalAttachmentPrepareError {
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for GraphPhysicalAttachmentPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reader(error) => write!(formatter, "prepare graph reader attachment: {error}"),
            Self::Writer(error) => write!(formatter, "prepare graph writer attachment: {error}"),
        }
    }
}

impl Error for GraphPhysicalAttachmentPrepareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reader(error) => Some(error),
            Self::Writer(error) => Some(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphRuntimePhysicalSnapshot {
    pub healthy: bool,
    pub writer_present: bool,
    pub reader_handles: u32,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub wal_bytes: u64,
}

/// Opaque owner of the native handles opened by the gated graph publisher.
pub struct GraphRuntimePhysicalAttachment {
    state: Mutex<GraphRuntimePhysicalState>,
}

struct GraphRuntimePhysicalState {
    binding: StoreRuntimeBindingV1,
    database_path: PathBuf,
    opened_file_identity: u64,
    initialization_file: Option<OpenedDatabaseFile>,
    writer: Option<Arc<PersistentWriter>>,
    readers: Option<ReaderPool<GraphReaderExecutor>>,
    admission_open: bool,
    drained: bool,
    closed: bool,
    close_failure: Option<String>,
}

impl GraphRuntimePhysicalAttachment {
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
            .ok_or_else(|| "graph attachment has no pending initialization".to_owned())?;
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
            return Err(MigrationSqlError::ReaderUnavailable(
                "graph physical attachment is closed".to_owned(),
            ));
        }
        let readers = state.readers.as_ref().ok_or_else(|| {
            MigrationSqlError::ReaderUnavailable("graph readers are unavailable".to_owned())
        })?;
        match state.writer.as_deref() {
            Some(writer) => MigrationSqlHandle::attach(writer, readers),
            None => Ok(MigrationSqlHandle::attach_read_only(readers)),
        }
    }

    pub fn snapshot(&self) -> GraphRuntimePhysicalSnapshot {
        let state = self.lock_state();
        let writer = state.writer.as_ref();
        let writer_telemetry = writer.map(|writer| writer.telemetry_snapshot());
        let readers = state.readers.as_ref().map(ReaderPool::snapshot);
        let reader_handles = readers.map_or(0, |snapshot| {
            u32::from(snapshot.general_workers) + u32::from(snapshot.health_workers)
        });
        GraphRuntimePhysicalSnapshot {
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
    ) -> Result<RuntimeSubmitOutcomeV1, GraphDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(GraphDispatchError::Closed);
            }
            state.writer.clone().ok_or(GraphDispatchError::Closed)?
        };
        writer
            .submit_authorized(request, probe, authority)
            .await
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        max_pages: u32,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<(), GraphDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(GraphDispatchError::Closed);
            }
            state.writer.clone().ok_or(GraphDispatchError::Closed)?
        };
        writer
            .bounded_incremental_vacuum(max_pages, authority)
            .await
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))
    }

    pub async fn run_checkpoint(
        &self,
        request: CheckpointRequest,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<CheckpointOutcome, GraphDispatchError> {
        let checkpoint = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(GraphDispatchError::Closed);
            }
            state
                .writer
                .as_ref()
                .ok_or(GraphDispatchError::Closed)?
                .checkpoint_handle()
        };
        let ticket = checkpoint
            .trigger_authorized(request, authority)
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))?;
        ticket
            .wait()
            .await
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))
    }

    pub async fn snapshot_to(
        &self,
        destination: PathBuf,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<OnlineBackupReceipt, GraphDispatchError> {
        let writer = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(GraphDispatchError::Closed);
            }
            state.writer.clone().ok_or(GraphDispatchError::Closed)?
        };
        writer
            .snapshot_to(destination, authority)
            .await
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))
    }

    pub fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, GraphDispatchError> {
        let readers = {
            let state = self.lock_state();
            if !state.admission_open || state.closed {
                return Err(GraphDispatchError::Closed);
            }
            state.readers.clone().ok_or(GraphDispatchError::Closed)?
        };
        let mut reader = readers
            .acquire_for_dispatch(&request, probe)
            .map_err(GraphDispatchError::Reader)?;
        let mut snapshot = reader
            .begin_snapshot()
            .map_err(|error| GraphDispatchError::ReaderWorker(error.to_string()))?;
        snapshot
            .execute(request, probe)
            .map_err(GraphDispatchError::Reader)
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
                    "graph physical attachment did not quiesce within {ATTACHMENT_DRAIN_TIMEOUT:?}: {leased} leased readers and {queued} queued writes"
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
                return Err("graph writer is still serving a request".to_owned());
            }
        };
        let readers = state.readers.take();
        drop(readers);
        if let Some(writer) = writer
            && let Err(error) = writer.shutdown_and_join()
        {
            let message = format!("join graph writer: {error}");
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
            return Err("graph physical attachment must drain before close".to_owned());
        }
        if !state.drained {
            return Err("graph physical attachment has not completed drain".to_owned());
        }
        if state.writer.is_some() || state.readers.is_some() {
            return Err("graph physical attachment retained handles after drain".to_owned());
        }
        state.closed = true;
        Ok(())
    }

    fn lock_state(&self) -> MutexGuard<'_, GraphRuntimePhysicalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for GraphRuntimePhysicalAttachment {
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
pub enum GraphDispatchError {
    Closed,
    Reader(ReaderAcquireError),
    ReaderWorker(String),
    Writer(String),
}

impl fmt::Display for GraphDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("graph runtime is closed"),
            Self::Reader(error) => write!(formatter, "graph read failed: {error}"),
            Self::ReaderWorker(error) => write!(formatter, "graph snapshot failed: {error}"),
            Self::Writer(error) => write!(formatter, "graph write failed: {error}"),
        }
    }
}

impl Error for GraphDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reader(error) => Some(error),
            Self::Closed | Self::ReaderWorker(_) | Self::Writer(_) => None,
        }
    }
}

fn wal_bytes(database_path: &std::path::Path) -> u64 {
    let mut name = database_path.as_os_str().to_os_string();
    name.push("-wal");
    std::fs::metadata(PathBuf::from(name)).map_or(0, |metadata| metadata.len())
}

#[derive(Clone, Copy, Debug, Default)]
struct PrecutoverRejectingGraphWriterPersistence;

impl WriterPersistence for PrecutoverRejectingGraphWriterPersistence {
    fn lookup_idempotency(
        &mut self,
        _transaction: &Transaction<'_>,
        _binding: &StoreRuntimeBindingV1,
        _idempotency: &IdempotencyIdentityV1,
    ) -> Result<Option<StoreCommitReceiptV1>, StorageRuntimeErrorV1> {
        Err(precutover_write_rejected())
    }

    fn apply_and_record(
        &mut self,
        _savepoint: &mut Savepoint<'_>,
        _binding: &StoreRuntimeBindingV1,
        _request: &RuntimeSubmitRequestV1,
    ) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
        Err(precutover_write_rejected())
    }
}

fn precutover_write_rejected() -> StorageRuntimeErrorV1 {
    StorageRuntimeErrorV1::Infrastructure {
        operation: "pre-cutover graph attachment rejects repository writes".to_owned(),
    }
}

#[derive(Debug)]
pub enum GraphPhysicalAttachmentStartError {
    ImmutableInitialization,
    Locator(CodeShardLocatorError),
    Prepare(GraphPhysicalAttachmentPrepareError),
    Identity(crate::connection::OpenedDatabaseFileError),
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for GraphPhysicalAttachmentStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImmutableInitialization => {
                formatter.write_str("immutable graph snapshots cannot be initialized")
            }
            Self::Locator(error) => write!(formatter, "prepare graph locator: {error}"),
            Self::Prepare(error) => write!(formatter, "prepare graph attachment: {error}"),
            Self::Identity(error) => write!(formatter, "identify graph attachment: {error}"),
            Self::Reader(error) => write!(formatter, "start graph readers: {error}"),
            Self::Writer(error) => write!(formatter, "start graph writer: {error}"),
        }
    }
}

impl Error for GraphPhysicalAttachmentStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Locator(error) => Some(error),
            Self::Prepare(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Reader(error) => Some(error),
            Self::Writer(error) => Some(error),
            Self::ImmutableInitialization => None,
        }
    }
}

#[cfg(test)]
mod attachment_identity_tests {
    use std::fs;

    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

    use super::*;

    fn snapshot_binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.graph-identity",
                "profile_id": "profile.graph-identity",
                "scope": {
                    "kind": "code",
                    "project_id": "project.graph-identity",
                    "repository_id": "repository.graph-identity",
                    "scope": {
                        "kind": "snapshot",
                        "worktree_id": "worktree.graph-identity",
                        "snapshot_id": "snapshot.graph-identity"
                    }
                }
            },
            "incarnation": 2,
            "authority_epoch": 9
        }))
        .unwrap()
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
    fn readers_bind_pinned_file_across_a_b_a_path_swap() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("graph.sqlite3");
        let displaced = directory.path().join("graph-a.sqlite3");
        let replacement = directory.path().join("graph-b.sqlite3");
        create_identity_database(&path, "A");
        create_identity_database(&replacement, "B");
        let path = path.canonicalize().unwrap();
        let binding = snapshot_binding();
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(2).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        let physical =
            CodeShardPhysicalLocator::from_verified_existing(binding, locator, path.clone())
                .unwrap();

        let result = GraphPhysicalAttachmentFactory.attach_with_start_hook(
            &physical,
            AdmissionConfigV1::default(),
            &mut |stage| match stage {
                AttachmentWorkerStartStage::BeforeReaders => {
                    fs::rename(&path, &displaced).unwrap();
                    fs::rename(&replacement, &path).unwrap();
                }
                AttachmentWorkerStartStage::AfterReaders => {
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
        assert!(matches!(
            error,
            GraphPhysicalAttachmentStartError::Reader(_)
        ));
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
}
