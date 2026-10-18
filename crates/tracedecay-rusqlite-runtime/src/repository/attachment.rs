use asupersync::{Cx, runtime::RuntimeHandle};
use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    thread,
    time::{Duration, Instant},
};

use rusqlite::Transaction;
use tracedecay_store::{
    AdmissionConfigV1, ConsistencyModeV1, RuntimeReadCoverageV1, RuntimeReadOperationV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1,
    UnavailableReasonV1, VerifiedStoreLocatorV1, single_shard_required_coverage_v1,
};

use crate::{
    CheckpointOutcome, CheckpointRequest, ExistingWriterLocator, PersistentWriter,
    RuntimeWriteAuthority, WriterActorError, WriterStartError, WriterState,
    connection::{OpenedDatabaseFile, OpenedDatabaseFileError},
    exact_sql::{ExactSqlError, ExactSqlHandle},
    ledger::{self, LedgerError},
    reader::{
        ExistingReaderLocator, ReaderAcquireError, ReaderCheckpointBlockers, ReaderPool,
        ReaderQueryExecutor, ReaderStartError,
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
    #[tracing::instrument(
        name = "rusqlite.attachment.attach_read_only",
        level = "trace",
        skip_all
    )]
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
            state: Arc::new(Mutex::new(RepositoryRuntimePhysicalState {
                binding,
                database_path: path,
                opened_file_identity: expected_identity,
                initialization_file: None,
                writer: None,
                readers: Some(readers),
                admission_open: true,
                drained: false,
                closed: false,
                file_lost: false,
                lost_readers: None,
                lost_writer: None,
                #[cfg(test)]
                after_lost_notice: None,
                close_failure: None,
            })),
            drain_serial: Mutex::new(()),
        })
    }

    pub fn attach(
        &self,
        runtime: &RuntimeHandle,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        self.attach_with_start_hook(runtime, binding, locator, path, admission, &mut |_| {})
    }

    fn attach_with_start_hook(
        &self,
        runtime: &RuntimeHandle,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
        start_hook: &mut dyn FnMut(AttachmentWorkerStartStage),
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        let opened_database =
            OpenedDatabaseFile::pin(&path).map_err(RepositoryAttachmentStartError::Identity)?;
        self.attach_opened(
            runtime,
            binding,
            locator,
            path,
            admission,
            opened_database,
            false,
            start_hook,
        )
    }

    #[tracing::instrument(name = "rusqlite.attachment.initialize", level = "trace", skip_all)]
    pub fn initialize(
        &self,
        runtime: &RuntimeHandle,
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
        admission: AdmissionConfigV1,
    ) -> Result<RepositoryRuntimePhysicalAttachment, RepositoryAttachmentStartError> {
        let opened_database = OpenedDatabaseFile::create_new(&path)
            .map_err(RepositoryAttachmentStartError::Identity)?;
        self.attach_opened(
            runtime,
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
    #[tracing::instrument(name = "rusqlite.attachment.attach", level = "trace", skip_all)]
    fn attach_opened(
        &self,
        runtime: &RuntimeHandle,
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
        let checkpoint_blockers = ReaderCheckpointBlockers::default();
        let lost_state = Arc::new(OnceLock::<Weak<Mutex<RepositoryRuntimePhysicalState>>>::new());
        let notice_state = Arc::clone(&lost_state);
        start_hook(AttachmentWorkerStartStage::BeforeWriter);
        let writer_result = PersistentWriter::start_with_checkpoint_blockers(
            runtime,
            writer_locator,
            admission.clone(),
            ConcreteRepositoryWriteExecutor::default(),
            Arc::new(checkpoint_blockers.clone()),
            Some(Box::new(move || {
                if let Some(state) = notice_state.get().and_then(Weak::upgrade) {
                    close_lost_file(&state);
                }
            })),
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
        let readers_result = ReaderPool::start_with_checkpoint_control(
            reader_locator,
            admission.readers,
            RepositoryRuntimeReadExecutor::default(),
            Some(writer.checkpoint_handle().pressure_subscription()),
            checkpoint_blockers,
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
        let state = Arc::new(Mutex::new(RepositoryRuntimePhysicalState {
            binding,
            database_path: path,
            opened_file_identity,
            initialization_file,
            writer: Some(Arc::new(writer)),
            readers: Some(readers),
            admission_open: true,
            drained: false,
            closed: false,
            file_lost: false,
            lost_readers: None,
            lost_writer: None,
            #[cfg(test)]
            after_lost_notice: None,
            close_failure: None,
        }));
        let _ = lost_state.set(Arc::downgrade(&state));
        Ok(RepositoryRuntimePhysicalAttachment {
            state,
            drain_serial: Mutex::new(()),
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
    state: Arc<Mutex<RepositoryRuntimePhysicalState>>,
    /// Serializes `drain` so the state lock can be released across the writer
    /// join: the writer's lost-file notice takes that lock.
    drain_serial: Mutex<()>,
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
    file_lost: bool,
    /// The reader pool of a lost file, kept until its workers have exited so
    /// the registry can tell when no connection to the old file remains.
    lost_readers: Option<ReaderPool<RepositoryRuntimeReadExecutor>>,
    /// The writer of a lost file. Its task is still returning when the notice
    /// runs, and its connection closes only then, so it is kept until the task
    /// has finished.
    lost_writer: Option<Arc<PersistentWriter>>,
    /// Test-only: runs on the writer task after the notice, holding its
    /// connection open for as long as it blocks.
    #[cfg(test)]
    after_lost_notice: Option<Box<dyn FnOnce() + Send>>,
    close_failure: Option<String>,
}

/// Runs on the writer's pool task once its probe finds the file unlinked or
/// replaced: the writer already stopped, so this releases the readers and the
/// attachment's handles without waiting on any of them. The handles move into
/// the lost slots under the same lock that marks the file lost, so
/// `lost_file_closed` never sees a lost file with nothing left to wait on.
fn close_lost_file(state: &Mutex<RepositoryRuntimePhysicalState>) {
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.file_lost = true;
    guard.admission_open = false;
    if let Some(readers) = guard.readers.take() {
        readers.close_for_lost_file();
        guard.lost_readers = Some(readers);
    }
    guard.lost_writer = guard.writer.take();
    #[cfg(test)]
    {
        let hook = guard.after_lost_notice.take();
        drop(guard);
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl RepositoryRuntimePhysicalAttachment {
    /// Whether the store's file was unlinked or replaced under this
    /// attachment, which then closed every connection to it.
    pub fn file_lost(&self) -> bool {
        self.lock_state().file_lost
    }

    /// Whether a lost file has no open connection left: the writer task
    /// finished and every reader worker exited. Also true for an attachment
    /// whose file was never lost.
    pub fn lost_file_closed(&self) -> bool {
        let mut state = self.lock_state();
        if state
            .lost_readers
            .as_ref()
            .is_some_and(|readers| !readers.workers_exited())
            || state
                .lost_writer
                .as_ref()
                .is_some_and(|writer| !writer.worker_exited())
        {
            return false;
        }
        state.lost_readers = None;
        state.lost_writer = None;
        true
    }

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
        let readers = state.readers.as_ref();
        let occupancy = readers.map(ReaderPool::snapshot);
        let admission = readers.map(ReaderPool::telemetry_snapshot);
        let reader_handles = occupancy.map_or(0, |snapshot| {
            u32::from(snapshot.general_workers) + u32::from(snapshot.health_workers)
        });
        RepositoryRuntimePhysicalSnapshot {
            healthy: !state.file_lost
                && writer.is_none_or(|writer| writer.state() != WriterState::Faulted),
            writer_present: writer.is_some(),
            reader_handles,
            general_reader_waiters: occupancy.map_or(0, |snapshot| snapshot.waiting_general),
            health_reader_waiters: occupancy.map_or(0, |snapshot| snapshot.waiting_health),
            queued_operations: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_operations),
            queued_bytes: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.queue.queued_bytes),
            writer_busy_events: writer_telemetry
                .as_ref()
                .map_or(0, |snapshot| snapshot.busy_events),
            writer: writer
                .zip(writer_telemetry.as_ref())
                .map(|(writer, snapshot)| RepositoryWriterRuntimeSnapshot {
                    operations: snapshot.operations,
                    batches: snapshot.batches,
                    error_events: snapshot.error_events,
                    health_lane_services: snapshot.health_lane_services,
                    commit_sequence: snapshot.commit_sequence,
                    transactions: snapshot.transactions,
                    sqlite_vm: snapshot.sqlite_vm,
                    wal: snapshot.wal,
                    lock_work: snapshot.lock_work,
                    checkpoint_status: writer.checkpoint_handle().status(),
                    checkpoint_pressure: writer.checkpoint_handle().pressure(),
                }),
            wal_bytes: wal_bytes(&state.database_path),
            snapshot_admissions: occupancy.map_or(0, |snapshot| snapshot.snapshot_admissions),
            active_readers: occupancy.map_or(0, |snapshot| {
                snapshot
                    .leased_general
                    .saturating_add(snapshot.leased_health)
            }),
            reader_wait_micros: admission.map_or(0, |snapshot| snapshot.wait_micros),
            reader_execution_micros: admission.map_or(0, |snapshot| snapshot.execution_micros),
        }
    }

    pub async fn dispatch_submit(
        &self,
        cx: &Cx,
        request: RuntimeSubmitRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
    ) -> Result<RuntimeSubmitOutcomeV1, RepositoryDispatchError> {
        let writer = {
            let state = self.lock_state();
            if state.file_lost {
                return Ok(RuntimeSubmitOutcomeV1::Unavailable {
                    reason: UnavailableReasonV1::FileLost,
                });
            }
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .writer
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        writer
            .submit_authorized(cx, request, probe, authority)
            .await
            .map_err(|error| match error {
                WriterActorError::StorageFailure(error) => {
                    RepositoryDispatchError::WriterRuntime(error)
                }
                error => RepositoryDispatchError::Writer(error.to_string()),
            })
    }

    pub async fn run_bounded_incremental_compaction(
        &self,
        cx: &Cx,
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
            .bounded_incremental_vacuum(cx, max_pages, authority)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub async fn run_checkpoint(
        &self,
        cx: &Cx,
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
            .wait(cx)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    /// Runs one typed read as a job on the blocking pool, so the awaiting
    /// task's thread never parks on reader admission or a reader reply. The
    /// probe bounds the job: its deadline or cancellation interrupts the
    /// reader connection.
    pub async fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Result<RuntimeReadOutcomeV1, RepositoryDispatchError> {
        let readers = {
            let state = self.lock_state();
            if state.file_lost {
                return RuntimeReadOutcomeV1::new(
                    None,
                    RuntimeReadCoverageV1::Unavailable {
                        coverage: None,
                        reason: UnavailableReasonV1::FileLost,
                    },
                )
                .map_err(|error| RepositoryDispatchError::ReaderWorker(error.to_string()));
            }
            if !state.admission_open || state.closed {
                return Err(RepositoryDispatchError::Closed);
            }
            state
                .readers
                .clone()
                .ok_or(RepositoryDispatchError::Closed)?
        };
        crate::native_runtime::spawn_blocking(move || {
            let mut reader = readers
                .acquire_for_dispatch(&request, probe.as_ref())
                .map_err(RepositoryDispatchError::Reader)?;
            let mut snapshot = reader
                .begin_snapshot()
                .map_err(|error| RepositoryDispatchError::ReaderWorker(error.to_string()))?;
            snapshot
                .execute(request, probe.as_ref())
                .map_err(RepositoryDispatchError::Reader)
        })
        .await
        .map_err(RepositoryDispatchError::ReaderWorker)?
    }

    #[tracing::instrument(name = "rusqlite.attachment.drain", level = "trace", skip_all)]
    pub fn drain(&self) -> Result<(), String> {
        if crate::storage_pool::on_pool() {
            return Err(
                "repository physical attachment cannot drain on a storage pool thread".to_owned(),
            );
        }
        let _serial = self
            .drain_serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                let queued = state.writer.as_ref().map_or(0, |writer| {
                    writer.telemetry_snapshot().queue.queued_operations
                });
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
        drop(state);
        drop(readers);
        if let Some(writer) = writer
            && let Err(error) = writer.shutdown_and_join()
        {
            let message = format!("join repository writer: {error}");
            self.lock_state().close_failure = Some(message.clone());
            return Err(message);
        }
        self.lock_state().drained = true;
        Ok(())
    }

    #[tracing::instrument(name = "rusqlite.attachment.close", level = "trace", skip_all)]
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
        let pending = self.lock_state().initialization_file.is_some();
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
    WriterRuntime(StorageRuntimeErrorV1),
}

impl fmt::Display for RepositoryDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("repository runtime is closed"),
            Self::Reader(error) => write!(formatter, "repository read failed: {error}"),
            Self::ReaderWorker(error) => write!(formatter, "repository snapshot failed: {error}"),
            Self::Writer(error) => write!(formatter, "repository write failed: {error}"),
            Self::WriterRuntime(error) => write!(formatter, "repository write failed: {error}"),
        }
    }
}

impl Error for RepositoryDispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reader(error) => Some(error),
            Self::Closed | Self::ReaderWorker(_) | Self::Writer(_) => None,
            Self::WriterRuntime(error) => Some(error),
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
        let coverage = match request.consistency() {
            ConsistencyModeV1::LatestAvailable => RuntimeReadCoverageV1::Latest { observed: None },
            ConsistencyModeV1::AtLeast { commit_sequence } => {
                let (observed, reason) =
                    match ledger::current_watermark(snapshot, request.binding()) {
                        Ok(observed) => (observed, UnavailableReasonV1::MissingAuthority),
                        Err(LedgerError::StaleAuthority { .. }) => {
                            (None, UnavailableReasonV1::WrongAuthorityEpoch)
                        }
                        Err(error) => {
                            return Err(infrastructure(format!(
                                "read repository commit watermark: {error}"
                            )));
                        }
                    };
                let coverage = single_shard_required_coverage_v1(
                    request.binding(),
                    *commit_sequence,
                    observed.clone(),
                )
                .map_err(|error| {
                    infrastructure(format!("construct repository read coverage: {error}"))
                })?;
                if observed.is_none() {
                    RuntimeReadCoverageV1::Unavailable {
                        coverage: Some(coverage),
                        reason,
                    }
                } else if coverage.is_complete() {
                    RuntimeReadCoverageV1::Complete { coverage }
                } else {
                    RuntimeReadCoverageV1::Stale { coverage }
                }
            }
        };
        let value = if matches!(
            coverage,
            RuntimeReadCoverageV1::Stale { .. } | RuntimeReadCoverageV1::Unavailable { .. }
        ) {
            None
        } else {
            Some(match request.operation() {
                RuntimeReadOperationV1::TemporalHealth => {
                    let healthy = snapshot
                        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                        .map(|value| value.eq_ignore_ascii_case("ok"))
                        .map_err(|error| {
                            infrastructure(format!("repository quick check: {error}"))
                        })?;
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
            })
        };
        RuntimeReadOutcomeV1::new(value, coverage)
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
    use std::{fs, sync::mpsc, thread, time::Duration};

    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{AdmissionConfigV1, StoreIncarnationV1};

    use crate::exact_sql::{ExactSqlError, ExactSqlStatement, ExactSqlValue};

    use asupersync::channel::watch;

    use super::*;

    fn native_runtime() -> RuntimeHandle {
        crate::native_runtime::handle().expect("native runtime")
    }

    fn cx() -> Cx {
        crate::native_runtime::cx().expect("native runtime context")
    }

    async fn wait_for<T>(
        cx: &Cx,
        receiver: &mut watch::Receiver<T>,
        mut predicate: impl FnMut(&T) -> bool,
    ) -> Result<(), watch::RecvError> {
        loop {
            if predicate(&receiver.borrow_and_update()) {
                return Ok(());
            }
            receiver.changed(cx).await?;
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
            &native_runtime(),
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

    /// This process's threads whose name starts with `prefix`, compared on
    /// the 15 bytes Linux keeps of a thread name.
    #[cfg(target_os = "linux")]
    fn threads_named(prefix: &str) -> usize {
        let kept = &prefix[..prefix.len().min(15)];
        fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|task| fs::read_to_string(task.ok()?.path().join("comm")).ok())
            .filter(|name| name.trim_end().starts_with(kept))
            .count()
    }

    #[cfg(target_os = "linux")]
    fn threads() -> std::collections::BTreeMap<String, String> {
        fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|task| {
                let task = task.ok()?;
                let name = fs::read_to_string(task.path().join("comm")).ok()?;
                Some((
                    task.file_name().to_string_lossy().into_owned(),
                    name.trim_end().to_owned(),
                ))
            })
            .collect()
    }

    fn attach_seeded_store(
        directory: &std::path::Path,
        binding: &StoreRuntimeBindingV1,
        index: usize,
    ) -> RepositoryRuntimePhysicalAttachment {
        let path = directory.join(format!("store-{index}.sqlite3"));
        create_identity_database(&path, "seed");
        RepositoryPhysicalAttachmentFactory
            .attach(
                &native_runtime(),
                binding.clone(),
                locator(binding),
                path.canonicalize().unwrap(),
                AdmissionConfigV1::default(),
            )
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    fn alone_in_child_process(test: &str) -> bool {
        const CHILD: &str = "TRACEDECAY_ATTACHMENT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        let test = format!("{}::{test}", module_path!().split_once("::").unwrap().1);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test.as_str(), "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{test} failed in its child process:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sixty_four_attached_stores_run_on_the_fixed_storage_pool() {
        if !alone_in_child_process("sixty_four_attached_stores_run_on_the_fixed_storage_pool") {
            return;
        }
        let directory = TempDir::new().unwrap();
        let binding = binding();
        let exercise = |index: usize| {
            let attachment = attach_seeded_store(directory.path(), &binding, index);
            let handle = attachment.exact_sql_handle().unwrap();
            handle
                .execute(statement(
                    "INSERT INTO identity_probe (value) VALUES (?)",
                    vec![ExactSqlValue::Text(format!("store-{index}"))],
                ))
                .unwrap();
            let rows = handle
                .query(
                    statement("SELECT count(*) FROM identity_probe", vec![]),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(rows.rows[0].values, vec![ExactSqlValue::Integer(2)]);
            attachment
        };
        native_runtime();
        let before = threads();
        let attachments = (0..64).map(exercise).collect::<Vec<_>>();
        let started = threads()
            .into_iter()
            .filter(|(id, _)| !before.contains_key(id))
            .map(|(_, name)| name)
            .collect::<Vec<_>>();

        let pool = &crate::storage_pool::STORE_THREAD_PREFIX[..15];
        assert!(
            started.len() == crate::storage_pool::store_threads()
                && started.iter().all(|name| name.starts_with(pool)),
            "64 stores may start the storage pool's threads and no other: {started:?}"
        );
        assert_eq!(
            threads_named("tracedecay-rusqlite"),
            0,
            "no attached store may own a writer or reader thread"
        );
        assert_eq!(
            threads_named(crate::storage_pool::STORE_THREAD_PREFIX),
            crate::storage_pool::store_threads(),
            "every store's writer and readers run on the fixed pool"
        );
        for attachment in &attachments {
            attachment.drain().unwrap();
            attachment.close_and_join().unwrap();
        }
    }

    #[test]
    fn a_pool_task_is_refused_synchronous_store_calls_instead_of_parked() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_seeded_store(directory.path(), &binding(), 0);
        let handle = attachment.exact_sql_handle().unwrap();
        let on_pool = handle.clone();
        let (report, refused) = mpsc::channel();
        crate::storage_pool::spawn(async move {
            let write = on_pool.execute(statement(
                "INSERT INTO identity_probe (value) VALUES ('from the pool')",
                vec![],
            ));
            let read = on_pool.query(
                statement("SELECT count(*) FROM identity_probe", vec![]),
                Duration::from_secs(10),
            );
            let _ = report.send((write.map(|_| ()), read.map(|_| ())));
        })
        .unwrap_or_else(|_| panic!("the storage pool must start"));

        let (write, read) = refused
            .recv_timeout(Duration::from_secs(10))
            .expect("a pool task's synchronous store calls must return, not park");
        assert_eq!(write, Err(ExactSqlError::WriterUnavailable));
        assert!(read.is_err(), "a pool task's synchronous read must refuse");
        let rows = handle
            .query(
                statement("SELECT count(*) FROM identity_probe", vec![]),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(
            rows.rows[0].values,
            vec![ExactSqlValue::Integer(1)],
            "a refused write must not reach the writer"
        );
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    #[test]
    fn a_pool_task_is_refused_a_drain_and_a_writer_join_while_the_close_completes() {
        let directory = TempDir::new().unwrap();
        let attachment = Arc::new(attach_seeded_store(directory.path(), &binding(), 0));
        let donor = attach_seeded_store(directory.path(), &binding(), 1);
        let writer = Arc::try_unwrap(donor.lock_state().writer.take().unwrap())
            .unwrap_or_else(|_| panic!("only its attachment holds the writer"));
        let writer_status = writer.checkpoint_handle().status_subscription();
        let on_pool = Arc::clone(&attachment);
        let (report, refused) = mpsc::channel();
        crate::storage_pool::spawn(async move {
            let _ = report.send((on_pool.drain(), writer.shutdown_and_join()));
        })
        .unwrap_or_else(|_| panic!("the storage pool must start"));

        let (drain, join) = refused
            .recv_timeout(Duration::from_secs(10))
            .expect("a pool task's drain and writer join must return, not park");
        assert!(drain.is_err(), "a pool task's drain must refuse");
        attachment
            .exact_sql_handle()
            .expect("a refused drain leaves the attachment open")
            .execute(statement(
                "INSERT INTO identity_probe (value) VALUES ('after the refused drain')",
                vec![],
            ))
            .expect("a refused drain leaves the writer serving");
        assert!(
            matches!(join, Err(crate::WriterActorError::WaitCancelled)),
            "a pool task's writer join must report the wait it refused, got {join:?}"
        );
        let closing = std::time::Instant::now();
        while !writer_status.is_closed() {
            assert!(
                closing.elapsed() < Duration::from_secs(10),
                "the writer whose join was refused must still close"
            );
            thread::sleep(Duration::from_millis(5));
        }
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
        donor.drain().unwrap();
        donor.close_and_join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_lost_file_is_not_closed_while_the_lost_writer_still_holds_its_connection() {
        let directory = TempDir::new().unwrap();
        let attachment = Arc::new(attach_seeded_store(directory.path(), &binding(), 0));
        let (hold, held) = mpsc::channel::<()>();
        attachment.lock_state().after_lost_notice = Some(Box::new(move || {
            let _ = held.recv();
        }));
        fs::remove_file(directory.path().join("store-0.sqlite3")).unwrap();

        let deadline =
            std::time::Instant::now() + crate::FILE_PROBE_INTERVAL + Duration::from_secs(10);
        while !attachment
            .lock_state()
            .lost_readers
            .as_ref()
            .is_some_and(ReaderPool::workers_exited)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the lost file's readers never exited"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(attachment.file_lost());
        let (answer, answered) = mpsc::channel();
        let asking = Arc::clone(&attachment);
        thread::spawn(move || {
            let _ = answer.send(asking.lost_file_closed());
        });
        assert_eq!(
            answered.recv_timeout(Duration::from_secs(5)),
            Ok(false),
            "the writer task is still running, so its connection is still open"
        );

        hold.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !attachment.lost_file_closed() {
            assert!(
                std::time::Instant::now() < deadline,
                "the file stayed unclosed after the writer task returned"
            );
            thread::sleep(Duration::from_millis(5));
        }
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn drain_completes_when_the_probe_finds_the_file_gone_while_the_writer_is_busy() {
        let directory = TempDir::new().unwrap();
        let attachment = Arc::new(attach_seeded_store(directory.path(), &binding(), 0));
        let handle = attachment.exact_sql_handle().unwrap();
        let busy = thread::spawn(move || {
            handle.execute(statement(
                "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 2000000000) \
                 SELECT count(*) FROM c",
                vec![],
            ))
        });
        thread::sleep(Duration::from_millis(200));
        fs::remove_file(directory.path().join("store-0.sqlite3")).unwrap();
        thread::sleep(crate::FILE_PROBE_INTERVAL);

        let (done, drained) = mpsc::channel();
        let draining = Arc::clone(&attachment);
        thread::spawn(move || {
            let _ = done.send(draining.drain());
        });
        let drain = drained.recv_timeout(Duration::from_secs(30)).expect(
            "a drain that joins the writer must not wait on the lost-file notice's state lock",
        );
        assert_eq!(drain, Ok(()));
        let _ = busy.join();
    }

    /// This process's descriptors on files under `directory` that were unlinked.
    #[cfg(target_os = "linux")]
    fn deleted_descriptors(directory: &std::path::Path) -> Vec<String> {
        let prefix = directory.display().to_string();
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
            .map(|target| target.display().to_string())
            .filter(|target| target.starts_with(&prefix) && target.ends_with(" (deleted)"))
            .collect()
    }

    /// A reader lease or retained snapshot holds the pool, so the pool must
    /// stop holding the unlinked inode itself: no worker connection, and no
    /// pinned descriptor.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_lost_file_closes_every_descriptor_while_a_reader_pool_is_still_held() {
        let directory = TempDir::new().unwrap();
        let attachment = attach_seeded_store(directory.path(), &binding(), 0);
        let held = attachment
            .lock_state()
            .readers
            .clone()
            .expect("an attached store has readers");
        fs::remove_file(directory.path().join("store-0.sqlite3")).unwrap();

        let deadline =
            std::time::Instant::now() + crate::FILE_PROBE_INTERVAL + Duration::from_secs(10);
        while !(attachment.file_lost()
            && attachment.lost_file_closed()
            && deleted_descriptors(directory.path()).is_empty())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "a held pool must not keep the unlinked file open: lost={} closed={} {:?}",
                attachment.file_lost(),
                attachment.lost_file_closed(),
                deleted_descriptors(directory.path())
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(held.workers_exited());
        drop(held);
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
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
                    &native_runtime(),
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
            assert_eq!(
                fs::metadata(&wal_path).map_or(0, |metadata| metadata.len()),
                0,
                "a graceful close must return the whole WAL to the database"
            );
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

    #[test]
    fn hard_wal_pressure_queues_further_writes_until_the_snapshot_releases() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("hard-pressure.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        drop(connection);
        let path = path.canonicalize().unwrap();
        let mut wal_path = path.as_os_str().to_owned();
        wal_path.push("-wal");
        let wal_path = PathBuf::from(wal_path);
        let binding = binding();
        let mut admission = AdmissionConfigV1::default();
        admission.wal.soft_limit_bytes = 8 * 1024;
        admission.wal.hard_limit_bytes = 16 * 1024;
        let attachment = RepositoryPhysicalAttachmentFactory
            .attach(
                &native_runtime(),
                binding.clone(),
                locator(&binding),
                path,
                admission,
            )
            .unwrap();
        let handle = attachment.exact_sql_handle().unwrap();
        handle
            .execute_batch(
                "CREATE TABLE hard_pressure (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)"
                    .to_owned(),
            )
            .unwrap();
        let snapshot = handle.begin_read_snapshot(Duration::from_secs(1)).unwrap();
        snapshot
            .query(statement("SELECT COUNT(*) FROM hard_pressure", vec![]))
            .unwrap();

        handle
            .execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![
                    ExactSqlValue::Integer(1),
                    ExactSqlValue::Blob(vec![7; 128 * 1024]),
                ],
            ))
            .unwrap();
        let checkpoint = {
            let state = attachment.lock_state();
            state
                .writer
                .as_ref()
                .expect("writer remains attached")
                .checkpoint_handle()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut status = checkpoint.status_subscription();
        runtime
            .block_on(wait_for(
                &cx(),
                &mut checkpoint.pressure_subscription(),
                |pressure| matches!(pressure, crate::CheckpointPressure::BlockGeneral { .. }),
            ))
            .expect("the first over-hard-limit write must publish hard checkpoint pressure");
        let blocked_wal_bytes = std::fs::metadata(&wal_path).unwrap().len();

        let queued_handle = handle.clone();
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let queued = thread::spawn(move || {
            let result = queued_handle.execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![ExactSqlValue::Integer(2), ExactSqlValue::Blob(vec![8; 32])],
            ));
            let _ = completed_tx.send(result);
        });
        assert!(
            completed_rx
                .recv_timeout(Duration::from_millis(300))
                .is_err(),
            "hard pressure must hold the next admitted write behind the checkpoint drain"
        );
        assert_eq!(
            std::fs::metadata(&wal_path).unwrap().len(),
            blocked_wal_bytes,
            "the queued write must not grow the physical WAL while hard pressure is active"
        );
        let pressure = checkpoint.pressure();
        assert!(matches!(
            pressure,
            crate::CheckpointPressure::BlockGeneral { ref blockers, .. }
                if !blockers.is_clear()
        ));
        // Every hard-drain retry publishes a checkpoint status, so wait for
        // one instead of assuming the 100 ms retry timer fired in time.
        let writer_snapshot = loop {
            status.mark_seen();
            let writer_snapshot = attachment
                .snapshot()
                .writer
                .expect("the writer remains attached while hard pressure is active");
            if writer_snapshot.wal.hard_retry_wakes > 0 {
                break writer_snapshot;
            }
            runtime
                .block_on(status.changed(&cx()))
                .expect("the writer keeps retrying the hard drain");
        };
        assert!(matches!(
            writer_snapshot.checkpoint_pressure,
            crate::CheckpointPressure::BlockGeneral { .. }
        ));

        drop(snapshot);
        // The drain's checkpoint and the WAL restart behind it both fsync,
        // which a saturated disk stalls for tens of seconds; wait for the
        // write itself rather than bounding that I/O.
        completed_rx.recv().unwrap().unwrap();
        queued.join().unwrap();
        runtime
            .block_on(wait_for(&cx(), &mut status, |status| {
                matches!(checkpoint.pressure(), crate::CheckpointPressure::Open)
                    && status.latest.as_ref().is_some_and(|outcome| match outcome {
                        crate::CheckpointOutcome::BelowSoft { wal }
                        | crate::CheckpointOutcome::Complete { wal, .. } => wal.bytes <= 8 * 1024,
                        crate::CheckpointOutcome::Pending { .. }
                        | crate::CheckpointOutcome::Interrupted { .. } => false,
                    })
            }))
            .expect("the released reader must let the checkpoint recover below the soft limit");

        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    /// A reader that started at the newest frame never holds back the
    /// backfill, but it still holds a WAL read slot, and SQLite cannot restart
    /// the log while any slot is held. Releasing the drain once every frame is
    /// backfilled lets the next write append past the hard limit again; under
    /// overlapping reads that re-enters the drain on every batch and the log
    /// never restarts.
    #[test]
    fn hard_wal_pressure_holds_writes_until_the_wal_log_can_restart() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("hard-pressure-restart.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        drop(connection);
        let path = path.canonicalize().unwrap();
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let binding = binding();
        let mut admission = AdmissionConfigV1::default();
        admission.wal.soft_limit_bytes = 8 * 1024;
        admission.wal.hard_limit_bytes = 16 * 1024;
        let attachment = RepositoryPhysicalAttachmentFactory
            .attach(
                &native_runtime(),
                binding.clone(),
                locator(&binding),
                path,
                admission,
            )
            .unwrap();
        let handle = attachment.exact_sql_handle().unwrap();
        handle
            .execute_batch(
                "CREATE TABLE hard_pressure (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)"
                    .to_owned(),
            )
            .unwrap();
        let count = || statement("SELECT COUNT(*) FROM hard_pressure", vec![]);
        let older = handle.begin_read_snapshot(Duration::from_secs(1)).unwrap();
        older.query(count()).unwrap();
        handle
            .execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![
                    ExactSqlValue::Integer(1),
                    ExactSqlValue::Blob(vec![7; 128 * 1024]),
                ],
            ))
            .unwrap();
        let checkpoint = attachment
            .lock_state()
            .writer
            .as_ref()
            .expect("writer remains attached")
            .checkpoint_handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut pressure = checkpoint.pressure_subscription();
        runtime
            .block_on(wait_for(&cx(), &mut pressure, |pressure| {
                matches!(pressure, crate::CheckpointPressure::BlockGeneral { .. })
            }))
            .expect("the over-hard-limit write must publish hard checkpoint pressure");
        // Hard pressure blocks only general readers; the health lane still
        // admits, and a reader admitted now reads the newest frame.
        let current = handle
            .begin_health_read_snapshot(Duration::from_secs(1))
            .unwrap();
        current.query(count()).unwrap();
        let blocked_wal_bytes = fs::metadata(&wal_path).unwrap().len();

        let queued_handle = handle.clone();
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let queued = thread::spawn(move || {
            let result = queued_handle.execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![ExactSqlValue::Integer(2), ExactSqlValue::Blob(vec![8; 32])],
            ));
            let _ = completed_tx.send(result);
        });
        drop(older);
        assert!(
            completed_rx.recv_timeout(Duration::from_secs(2)).is_err(),
            "a reader still holding a WAL read slot must keep the drain: the log cannot restart"
        );
        assert_eq!(
            fs::metadata(&wal_path).unwrap().len(),
            blocked_wal_bytes,
            "the queued write must not grow the physical WAL while the log cannot restart"
        );
        runtime
            .block_on(wait_for(&cx(), &mut pressure, |pressure| {
                matches!(
                    pressure,
                    crate::CheckpointPressure::BlockGeneral { blockers, .. }
                        if blockers.count() == 1
                )
            }))
            .expect("the drain must name the one reader that still holds a read slot");

        drop(current);
        completed_rx.recv().unwrap().unwrap();
        queued.join().unwrap();
        let mut status = checkpoint.status_subscription();
        runtime
            .block_on(wait_for(&cx(), &mut status, |status| {
                status.latest.as_ref().is_some_and(|outcome| match outcome {
                    crate::CheckpointOutcome::BelowSoft { wal }
                    | crate::CheckpointOutcome::Complete { wal, .. } => wal.bytes <= 8 * 1024,
                    crate::CheckpointOutcome::Pending { .. }
                    | crate::CheckpointOutcome::Interrupted { .. } => false,
                })
            }))
            .expect("the write after the drain must start the restarted log from its first frame");

        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }

    /// #4097: a reader that never releases its snapshot used to keep the hard
    /// drain pending forever, with every general write refused. Past one read
    /// deadline the fence revokes the snapshot, the log restarts, and the
    /// queued write commits while the snapshot is still held.
    #[test]
    fn a_hard_drain_converges_behind_a_reader_that_never_releases() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("hard-pressure-revoke.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        drop(connection);
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let mut admission = AdmissionConfigV1::default();
        admission.wal.soft_limit_bytes = 8 * 1024;
        admission.wal.hard_limit_bytes = 16 * 1024;
        let attachment = RepositoryPhysicalAttachmentFactory
            .attach(
                &native_runtime(),
                binding.clone(),
                locator(&binding),
                path,
                admission,
            )
            .unwrap();
        let handle = attachment.exact_sql_handle().unwrap();
        handle
            .execute_batch(
                "CREATE TABLE hard_pressure (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)"
                    .to_owned(),
            )
            .unwrap();
        let count = || statement("SELECT COUNT(*) FROM hard_pressure", vec![]);
        let pinned = handle.begin_read_snapshot(Duration::from_secs(1)).unwrap();
        pinned.query(count()).unwrap();
        handle
            .execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![
                    ExactSqlValue::Integer(1),
                    ExactSqlValue::Blob(vec![7; 128 * 1024]),
                ],
            ))
            .unwrap();
        let checkpoint = attachment
            .lock_state()
            .writer
            .as_ref()
            .expect("writer remains attached")
            .checkpoint_handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut pressure = checkpoint.pressure_subscription();
        runtime
            .block_on(wait_for(&cx(), &mut pressure, |pressure| {
                matches!(pressure, crate::CheckpointPressure::BlockGeneral { .. })
            }))
            .expect("the over-hard-limit write must publish hard checkpoint pressure");

        let queued = handle.clone();
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            let result = queued.execute(statement(
                "INSERT INTO hard_pressure (id, payload) VALUES (?, ?)",
                vec![ExactSqlValue::Integer(2), ExactSqlValue::Blob(vec![8; 32])],
            ));
            let _ = completed_tx.send(result);
        });
        completed_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the queued write is not held until the reader lets go")
            .expect("the queued write commits while the reader still holds its snapshot");
        writer.join().unwrap();
        let mut status = checkpoint.status_subscription();
        let context = cx();
        let converged = wait_for(&context, &mut status, |status| {
            matches!(checkpoint.pressure(), crate::CheckpointPressure::Open)
                && status.latest.as_ref().is_some_and(|outcome| match outcome {
                    crate::CheckpointOutcome::BelowSoft { wal }
                    | crate::CheckpointOutcome::Complete { wal, .. } => wal.bytes <= 8 * 1024,
                    crate::CheckpointOutcome::Pending { .. }
                    | crate::CheckpointOutcome::Interrupted { .. } => false,
                })
        });
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(30), converged).await })
            .expect("the drain converges while the reader still holds its snapshot")
            .expect("the drain completes and the restarted log stays below the soft limit");
        assert!(
            pinned.query(count()).is_err(),
            "the revoked snapshot no longer reads its old view"
        );

        drop(pinned);
        attachment.drain().unwrap();
        attachment.close_and_join().unwrap();
    }
}
