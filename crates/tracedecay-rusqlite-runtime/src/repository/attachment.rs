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
    ShardWatermarkV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    CheckpointControlError, CheckpointOutcome, CheckpointRequest, ExistingWriterLocator,
    MaintenanceCheckpointRequest, OnlineBackupReceipt, PersistentWriter, RuntimeWriteAuthority,
    RuntimeWriteAuthorityStage, WriterStartError, WriterState,
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    exact_sql::{ExactSqlError, ExactSqlHandle},
    reader::{
        ExistingReaderLocator, ReaderAcquireError, ReaderPool, ReaderQueryExecutor,
        ReaderStartError,
    },
};

use super::{ConcreteRepositoryReadExecutor, ConcreteRepositoryWriteExecutor};

mod telemetry;

use telemetry::wal_bytes;
pub use telemetry::{RepositoryRuntimePhysicalSnapshot, RepositoryWriterRuntimeSnapshot};

const ATTACHMENT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACHMENT_DRAIN_POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Default)]
pub struct RepositoryPhysicalAttachmentFactory;

impl RepositoryPhysicalAttachmentFactory {
    pub fn attach_read_only(
        &self,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        let opened_database =
            OpenedDatabaseFile::pin(&path).map_err(RepositoryAttachmentStartError::Identity)?;
        let reader_locator = ExistingReaderLocator::new(binding.clone(), locator, path.clone())
            .map_err(RepositoryAttachmentStartError::Reader)?;
        let reader_locator = reader_locator.with_opened_database(
            opened_database
                .try_clone()
                .map_err(RepositoryAttachmentStartError::Identity)?,
        );
        let readers = ReaderPool::start_with_checkpoint_pressure(
            reader_locator,
            admission.readers,
            RepositoryRuntimeReadExecutor::default(),
            None,
        )
        .map_err(RepositoryAttachmentStartError::Reader)?;
        let expected_identity = opened_database.identity();
        if readers.opened_file_identity() != Some(expected_identity) {
            drop(readers);
            return Err(RepositoryAttachmentStartError::Identity(
                OpenedDatabaseFileError::Replaced,
            ));
        }
        opened_database
            .verify_current_path(&path)
            .map_err(RepositoryAttachmentStartError::Identity)?;
        Ok(RepositoryRuntimePhysicalAttachment {
            state: Mutex::new(RepositoryRuntimePhysicalState {
                binding,
                database_path: path,
                opened_file_identity: expected_identity,
                initialization_file: None,
                writer: None,
                readers: Some(readers),
                admission_open: true,
                drained: false,
                closed: false,
                close_failure: None,
            }),
        })
    }

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
    Identity(crate::connection::OpenedDatabaseFileError),
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for RepositoryAttachmentStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        }
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

    pub fn exact_sql_handle(&self) -> Result<ExactSqlHandle, ExactSqlError> {
        let state = self.lock_state();
        if !state.admission_open || state.closed {
            return Err(ExactSqlError::WriterUnavailable);
        }
        let readers = state.readers.as_ref().ok_or_else(|| {
            ExactSqlError::ReaderUnavailable("repository readers are unavailable".to_owned())
        })?;
        match state.writer.as_deref() {
            Some(writer) => ExactSqlHandle::attach(writer, readers),
            None => Ok(ExactSqlHandle::attach_read_only(readers)),
        }
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
            general_reader_waiters: readers.map_or(0, |snapshot| snapshot.waiting_general),
            health_reader_waiters: readers.map_or(0, |snapshot| snapshot.waiting_health),
            queued_operations: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_operations),
            queued_bytes: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_bytes),
            writer_busy_events: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.busy_events),
            writer: writer_telemetry
                .as_ref()
                .map(|snapshot| RepositoryWriterRuntimeSnapshot {
                    operations: snapshot.operations,
                    batches: snapshot.batches,
                    error_events: snapshot.error_events,
                    health_lane_services: snapshot.health_lane_services,
                    commit_sequence: snapshot.commit_sequence,
                }),
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

    /// Close admission and move the retained writer to `Draining` so an
    /// exclusive maintenance checkpoint can run. Unlike [`Self::drain`], the
    /// writer is neither taken nor joined: its checkpoint handle stays live
    /// for [`Self::run_maintenance_checkpoint`]. Idempotent while draining.
    ///
    /// Crate-private: public callers use [`Self::run_maintenance_checkpoint`],
    /// which validates permit and admission-stage authority first. Admission is
    /// not reopened; that exclusive window is intentional.
    ///
    /// Test modules in this crate call this wrapper; lib clippy does not see
    /// those `#[cfg(test)]` uses.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn begin_maintenance_drain(&self) -> Result<(), RepositoryDispatchError> {
        let mut state = self.lock_state();
        Self::begin_maintenance_drain_locked(&mut state)
    }

    fn begin_maintenance_drain_locked(
        state: &mut RepositoryRuntimePhysicalState,
    ) -> Result<(), RepositoryDispatchError> {
        if state.closed {
            return Err(RepositoryDispatchError::Closed);
        }
        let Some(writer) = state.writer.as_ref() else {
            return Err(RepositoryDispatchError::Closed);
        };
        writer.begin_drain();
        if let Some(readers) = &state.readers {
            readers.begin_drain();
        }
        state.admission_open = false;
        Ok(())
    }

    /// Run an exclusive WAL RESTART/TRUNCATE checkpoint through the retained
    /// writer. Maintenance runs after admission has closed, so this does not
    /// require `admission_open`; a still-`Ready` writer is moved to
    /// `Draining` first. PASSIVE checkpoints stay on [`Self::run_checkpoint`].
    /// Admission is not reopened after Truncate — the exclusive window lasts
    /// until `drain` + `close_and_join`.
    ///
    /// The permit binding, admission-stage authority, and request-local
    /// snapshot blockers are validated before any lifecycle transition:
    /// draining is irreversible, so a misrouted permit, revoked authority, or
    /// stale inventory (`Blocked`) must leave admission open and the writer
    /// `Ready`.
    pub async fn run_maintenance_checkpoint(
        &self,
        request: MaintenanceCheckpointRequest,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<CheckpointOutcome, RepositoryDispatchError> {
        let checkpoint = {
            let mut state = self.lock_state();
            if state.closed || state.writer.is_none() {
                return Err(RepositoryDispatchError::Closed);
            }
            if request.permit().binding() != &state.binding {
                return Err(RepositoryDispatchError::Checkpoint(
                    CheckpointControlError::BindingMismatch,
                ));
            }
            if authority
                .verify(RuntimeWriteAuthorityStage::BeforeAdmission)
                .is_err()
            {
                return Err(RepositoryDispatchError::Checkpoint(
                    CheckpointControlError::AuthorityDenied {
                        stage: RuntimeWriteAuthorityStage::BeforeAdmission,
                    },
                ));
            }
            if !request.blockers().is_clear() {
                return Err(RepositoryDispatchError::Checkpoint(
                    CheckpointControlError::Blocked(request.blockers().clone()),
                ));
            }
            Self::begin_maintenance_drain_locked(&mut state)?;
            state
                .writer
                .as_ref()
                .ok_or(RepositoryDispatchError::Closed)?
                .checkpoint_handle()
        };
        let ticket = checkpoint
            .trigger_maintenance_authorized(request, authority)
            .map_err(RepositoryDispatchError::Checkpoint)?;
        ticket
            .wait()
            .await
            .map_err(RepositoryDispatchError::Checkpoint)
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

    pub async fn snapshot_to_interruptible(
        &self,
        destination: PathBuf,
        probe: Arc<dyn RuntimeRequestProbeV1>,
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
            .snapshot_to_interruptible(destination, probe, authority)
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
    Checkpoint(CheckpointControlError),
    Reader(ReaderAcquireError),
    ReaderWorker(String),
    Writer(String),
}

impl fmt::Display for RepositoryDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("repository runtime is closed"),
            Self::Checkpoint(error) => {
                write!(
                    formatter,
                    "repository maintenance checkpoint failed: {error}"
                )
            }
            Self::Reader(error) => write!(formatter, "repository read failed: {error}"),
            Self::ReaderWorker(error) => write!(formatter, "repository snapshot failed: {error}"),
            Self::Writer(error) => write!(formatter, "repository write failed: {error}"),
        }
    }
}

impl Error for RepositoryDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Checkpoint(error) => Some(error),
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

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{AdmissionConfigV1, StoreIncarnationV1};

    use crate::{
        CheckpointBlockers, CheckpointKind, MaintenanceCheckpointMode, RuntimeWriteAuthorityError,
        exact_sql::{ExactSqlError, ExactSqlStatement, ExactSqlValue},
        maintenance::{ExclusiveMaintenancePermit, MaintenanceOwnerId},
    };

    use super::*;

    struct UnrestrictedAuthority;

    impl RuntimeWriteAuthority for UnrestrictedAuthority {
        fn verify(
            &self,
            _stage: RuntimeWriteAuthorityStage,
        ) -> Result<(), RuntimeWriteAuthorityError> {
            Ok(())
        }
    }

    struct DeniedAuthority;

    impl RuntimeWriteAuthority for DeniedAuthority {
        fn verify(
            &self,
            stage: RuntimeWriteAuthorityStage,
        ) -> Result<(), RuntimeWriteAuthorityError> {
            Err(RuntimeWriteAuthorityError::denied(format!(
                "maintenance authority denied at {stage:?}"
            )))
        }
    }

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

    fn statement(sql: &str, params: Vec<ExactSqlValue>) -> ExactSqlStatement {
        ExactSqlStatement::new(sql.to_owned(), params).unwrap()
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

    /// Unix-only: on Windows the writer's pin denies the swap-back rename
    /// outright, so this race cannot occur there;
    /// `connection::tests::windows_pinned_file_blocks_replacement_until_authority_closes`
    /// proves that stronger OS-level protection directly.
    #[cfg(unix)]
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
    fn real_sqlite_attachment_drains_pending_wal_reopens_and_rejects_stale_handles() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("repository.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        drop(connection);
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
            assert!(
                attachment.snapshot().writer.is_some(),
                "a writable attachment must expose its retained writer telemetry"
            );
            let handle = attachment.exact_sql_handle().unwrap();
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
                    vec![ExactSqlValue::Integer(cycle)],
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
                vec![ExactSqlValue::Integer(cycle)]
            );
            let wal_path = PathBuf::from(format!("{}-wal", path.display()));
            assert!(
                fs::metadata(&wal_path).unwrap().len() > 0,
                "each close cycle must begin with committed frames pending in WAL"
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
                    vec![ExactSqlValue::Integer(cycle + 10)],
                ))
                .unwrap_err();
            assert_eq!(write_error, ExactSqlError::WriterUnavailable);
            let read_error = handle
                .query(
                    statement("SELECT cycle FROM runtime_lifecycle", vec![]),
                    Duration::ZERO,
                )
                .unwrap_err();
            assert!(matches!(read_error, ExactSqlError::ReaderUnavailable(_)));

            let reopened = rusqlite::Connection::open(&path).unwrap();
            let count: i64 = reopened
                .query_row("SELECT COUNT(*) FROM runtime_lifecycle", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, cycle + 1);
        }
    }

    #[test]
    fn read_only_attachment_never_starts_or_exposes_a_writer() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("reader-only.sqlite3");
        create_identity_database(&path, "reader-only");
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let attachment = RepositoryPhysicalAttachmentFactory
            .attach_read_only(
                binding.clone(),
                locator(&binding),
                path,
                AdmissionConfigV1::default(),
            )
            .unwrap();

        let snapshot = attachment.snapshot();
        assert!(!snapshot.writer_present);
        assert!(snapshot.reader_handles > 0);
        assert_eq!(snapshot.general_reader_waiters, 0);
        assert_eq!(snapshot.health_reader_waiters, 0);
        assert_eq!(snapshot.writer_busy_events, 0);
        assert_eq!(snapshot.writer, None);
        let handle = attachment.exact_sql_handle().unwrap();
        let rows = handle
            .query(
                statement("SELECT value FROM identity_probe", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            rows.rows[0].values,
            vec![ExactSqlValue::Text("reader-only".to_owned())]
        );
        assert_eq!(
            handle
                .execute(statement(
                    "UPDATE identity_probe SET value = ?",
                    vec![ExactSqlValue::Text("write".to_owned())],
                ))
                .unwrap_err(),
            ExactSqlError::WriterUnavailable
        );

        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    fn attach_wal_database(directory: &TempDir) -> RepositoryRuntimePhysicalAttachment {
        let path = directory.path().join("maintenance.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        drop(connection);
        let path = path.canonicalize().unwrap();
        let binding = binding();
        RepositoryPhysicalAttachmentFactory
            .attach(
                binding.clone(),
                locator(&binding),
                path,
                AdmissionConfigV1::default(),
            )
            .unwrap()
    }

    fn maintenance_request(
        attachment: &RepositoryRuntimePhysicalAttachment,
        mode: MaintenanceCheckpointMode,
    ) -> MaintenanceCheckpointRequest {
        let permit = ExclusiveMaintenancePermit::issue(
            MaintenanceOwnerId::new(1).unwrap(),
            attachment.binding(),
        );
        MaintenanceCheckpointRequest::new(mode, permit, CheckpointBlockers::default())
    }

    fn foreign_binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.repository-lifecycle",
                "profile_id": "profile.repository-lifecycle",
                "scope": {
                    "kind": "project",
                    "project_id": "project.other-shard"
                }
            },
            "incarnation": 4,
            "authority_epoch": 12
        }))
        .unwrap()
    }

    fn assert_admission_open_and_writer_ready(attachment: &RepositoryRuntimePhysicalAttachment) {
        let state = attachment.lock_state();
        assert!(state.admission_open, "admission must stay open");
        assert!(!state.closed);
        assert_eq!(
            state
                .writer
                .as_ref()
                .expect("writer stays attached")
                .state(),
            WriterState::Ready,
            "writer must stay Ready"
        );
        assert!(state.readers.is_some());
    }

    #[test]
    fn maintenance_checkpoint_rejects_foreign_permit_before_draining() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_wal_database(&directory);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let foreign_permit = ExclusiveMaintenancePermit::issue(
            MaintenanceOwnerId::new(1).unwrap(),
            foreign_binding(),
        );
        let request = MaintenanceCheckpointRequest::new(
            MaintenanceCheckpointMode::Truncate,
            foreign_permit,
            CheckpointBlockers::default(),
        );
        let error = runtime
            .block_on(
                attachment.run_maintenance_checkpoint(request, Arc::new(UnrestrictedAuthority)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RepositoryDispatchError::Checkpoint(CheckpointControlError::BindingMismatch)
        ));

        assert_admission_open_and_writer_ready(&attachment);
        attachment
            .exact_sql_handle()
            .expect("a rejected foreign permit must not close exact-SQL admission");
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    #[test]
    fn maintenance_checkpoint_denied_authority_never_drains() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_wal_database(&directory);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let error = runtime
            .block_on(attachment.run_maintenance_checkpoint(
                maintenance_request(&attachment, MaintenanceCheckpointMode::Truncate),
                Arc::new(DeniedAuthority),
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            RepositoryDispatchError::Checkpoint(CheckpointControlError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeAdmission,
            })
        ));

        assert_admission_open_and_writer_ready(&attachment);
        attachment
            .exact_sql_handle()
            .expect("a denied authority must not close exact-SQL admission");
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    #[test]
    fn maintenance_checkpoint_preserves_typed_blocked_failure() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_wal_database(&directory);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let permit = ExclusiveMaintenancePermit::issue(
            MaintenanceOwnerId::new(1).unwrap(),
            attachment.binding(),
        );
        let blockers = CheckpointBlockers {
            blockers: Vec::new(),
            omitted: 1,
        };
        let request = MaintenanceCheckpointRequest::new(
            MaintenanceCheckpointMode::Truncate,
            permit,
            blockers.clone(),
        );
        let error = runtime
            .block_on(
                attachment.run_maintenance_checkpoint(request, Arc::new(UnrestrictedAuthority)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RepositoryDispatchError::Checkpoint(CheckpointControlError::Blocked(ref actual))
                if *actual == blockers
        ));

        assert_admission_open_and_writer_ready(&attachment);
        attachment
            .exact_sql_handle()
            .expect("a blocked inventory must not close exact-SQL admission");
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    #[test]
    fn maintenance_port_reports_closed_without_a_writer_or_after_close() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("reader-only.sqlite3");
        create_identity_database(&path, "reader-only");
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let read_only = RepositoryPhysicalAttachmentFactory
            .attach_read_only(
                binding.clone(),
                locator(&binding),
                path,
                AdmissionConfigV1::default(),
            )
            .unwrap();
        assert!(matches!(
            read_only.begin_maintenance_drain(),
            Err(RepositoryDispatchError::Closed)
        ));
        let request = maintenance_request(&read_only, MaintenanceCheckpointMode::Truncate);
        let error = runtime
            .block_on(
                read_only.run_maintenance_checkpoint(request, Arc::new(UnrestrictedAuthority)),
            )
            .unwrap_err();
        assert!(matches!(error, RepositoryDispatchError::Closed));
        read_only.drain().unwrap();
        read_only.close_and_join().unwrap();

        let writable = attach_wal_database(&directory);
        writable.drain().unwrap();
        writable.close_and_join().unwrap();
        assert!(matches!(
            writable.begin_maintenance_drain(),
            Err(RepositoryDispatchError::Closed)
        ));
        let request = maintenance_request(&writable, MaintenanceCheckpointMode::Truncate);
        let error = runtime
            .block_on(writable.run_maintenance_checkpoint(request, Arc::new(UnrestrictedAuthority)))
            .unwrap_err();
        assert!(matches!(error, RepositoryDispatchError::Closed));
    }

    #[test]
    fn maintenance_drain_keeps_writer_attached_and_closes_admission() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_wal_database(&directory);
        let handle = attachment.exact_sql_handle().unwrap();
        handle
            .execute_batch("CREATE TABLE maintenance_probe (value INTEGER NOT NULL)".to_owned())
            .unwrap();

        attachment.begin_maintenance_drain().unwrap();
        attachment.begin_maintenance_drain().unwrap();
        {
            let state = attachment.lock_state();
            assert!(!state.admission_open);
            assert!(!state.closed);
            let writer = state.writer.as_ref().expect("writer stays attached");
            assert_eq!(writer.state(), WriterState::Draining);
            assert!(state.readers.is_some());
        }
        assert!(attachment.snapshot().writer_present);
        let admission_error = match attachment.exact_sql_handle() {
            Err(error) => error,
            Ok(_) => panic!("maintenance drain must close exact-SQL admission"),
        };
        assert_eq!(admission_error, ExactSqlError::WriterUnavailable);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let outcome = runtime
            .block_on(attachment.run_maintenance_checkpoint(
                maintenance_request(&attachment, MaintenanceCheckpointMode::Restart),
                Arc::new(UnrestrictedAuthority),
            ))
            .unwrap();
        assert!(matches!(
            outcome,
            CheckpointOutcome::Complete {
                kind: CheckpointKind::Restart,
                ..
            }
        ));

        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
        {
            let state = attachment.lock_state();
            assert!(state.closed);
            assert!(state.writer.is_none());
            assert!(state.readers.is_none());
        }
    }

    #[test]
    fn maintenance_truncate_is_typed_pending_while_pinned_then_reclaims_wal() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_wal_database(&directory);
        let handle = attachment.exact_sql_handle().unwrap();
        handle
            .execute_batch("CREATE TABLE maintenance_probe (value INTEGER NOT NULL)".to_owned())
            .unwrap();

        // Pin an independent read snapshot at the current WAL mark, then
        // append frames the checkpoint cannot backfill while it lives.
        let (database_path, wal_path) = {
            let state = attachment.lock_state();
            (
                state.database_path.clone(),
                PathBuf::from(format!("{}-wal", state.database_path.display())),
            )
        };
        let mut pinned = rusqlite::Connection::open(&database_path).unwrap();
        let pinned_snapshot = pinned.transaction().unwrap();
        let pinned_rows: i64 = pinned_snapshot
            .query_row("SELECT COUNT(*) FROM maintenance_probe", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pinned_rows, 0);
        for value in 0..8_i64 {
            handle
                .execute(statement(
                    "INSERT INTO maintenance_probe (value) VALUES (?)",
                    vec![ExactSqlValue::Integer(value)],
                ))
                .unwrap();
        }
        let pinned_wal_bytes = fs::metadata(&wal_path).unwrap().len();
        assert!(
            pinned_wal_bytes > 0,
            "maintenance truncate must start with committed frames pending in WAL"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let outcome = runtime
            .block_on(attachment.run_maintenance_checkpoint(
                maintenance_request(&attachment, MaintenanceCheckpointMode::Truncate),
                Arc::new(UnrestrictedAuthority),
            ))
            .unwrap();
        let report = match outcome {
            CheckpointOutcome::Pending {
                kind: CheckpointKind::Truncate,
                report,
                ..
            } => report,
            other => panic!("a pinned WAL truncate must be typed Pending, got {other:?}"),
        };
        assert!(
            report.busy,
            "a truncate blocked by a pinned reader must report busy"
        );
        assert!(
            report.checkpointed_frames < report.log_frames,
            "pinned frames must stay unbackfilled: {} of {}",
            report.checkpointed_frames,
            report.log_frames
        );
        assert_eq!(
            fs::metadata(&wal_path).unwrap().len(),
            pinned_wal_bytes,
            "a busy truncate must not report reclaim the WAL file disproves"
        );

        drop(pinned_snapshot);
        drop(pinned);
        let outcome = runtime
            .block_on(attachment.run_maintenance_checkpoint(
                maintenance_request(&attachment, MaintenanceCheckpointMode::Truncate),
                Arc::new(UnrestrictedAuthority),
            ))
            .unwrap();
        let report = match outcome {
            CheckpointOutcome::Complete {
                kind: CheckpointKind::Truncate,
                report,
                ..
            } => report,
            other => panic!("a released WAL truncate must complete, got {other:?}"),
        };
        assert!(!report.busy);
        assert_eq!(report.checkpointed_frames, report.log_frames);
        assert_eq!(
            fs::metadata(&wal_path).unwrap().len(),
            0,
            "an exclusive TRUNCATE checkpoint must leave a zero-length WAL"
        );

        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }
}
