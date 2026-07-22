use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Savepoint, Transaction};
use tracedecay_store::{
    AdmissionConfigV1, IdempotencyIdentityV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, StorageRuntimeErrorV1,
    StoreCommitReceiptV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, WriterStartError, WriterState,
    reader::{ExistingReaderLocator, ReaderAcquireError, ReaderPool, ReaderStartError},
    writer::WriterPersistence,
};

use super::{CodeShardPhysicalLocator, GraphMutationExecutor, GraphReaderExecutor};

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
        let database_path = physical.path().to_path_buf();
        let parts = self
            .prepare(physical)
            .map_err(GraphPhysicalAttachmentStartError::Prepare)?;
        let GraphPhysicalAttachmentParts {
            binding,
            reader_locator,
            writer_locator,
            reader_executor,
            ..
        } = parts;
        let reader_budget = admission.readers.clone();
        let writer = writer_locator
            .map(|locator| {
                PersistentWriter::start_with_persistence(
                    locator,
                    admission,
                    Box::new(PrecutoverRejectingGraphWriterPersistence),
                )
                .map(Arc::new)
            })
            .transpose()
            .map_err(GraphPhysicalAttachmentStartError::Writer)?;
        let readers = ReaderPool::start(reader_locator, reader_budget, reader_executor)
            .map_err(GraphPhysicalAttachmentStartError::Reader)?;
        Ok(GraphRuntimePhysicalAttachment {
            state: Mutex::new(GraphRuntimePhysicalState {
                binding,
                database_path,
                writer,
                readers: Some(readers),
                admission_open: true,
                closed: false,
            }),
        })
    }
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
    writer: Option<Arc<PersistentWriter>>,
    readers: Option<ReaderPool<GraphReaderExecutor>>,
    admission_open: bool,
    closed: bool,
}

impl GraphRuntimePhysicalAttachment {
    pub fn binding(&self) -> StoreRuntimeBindingV1 {
        self.lock_state().binding.clone()
    }

    pub fn snapshot(&self) -> GraphRuntimePhysicalSnapshot {
        let state = self.lock_state();
        let writer = state.writer.as_ref();
        let writer_telemetry = writer.map(|writer| writer.telemetry_snapshot());
        let readers = state.readers.as_ref().map(ReaderPool::snapshot);
        let reader_handles = readers.map_or(0, |snapshot| {
            if state.admission_open {
                u32::from(snapshot.general_workers) + u32::from(snapshot.health_workers)
            } else {
                u32::from(snapshot.leased_general) + u32::from(snapshot.leased_health)
            }
        });
        GraphRuntimePhysicalSnapshot {
            healthy: writer.is_none_or(|writer| writer.state() != WriterState::Faulted),
            writer_present: state.admission_open && writer.is_some(),
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
    ) -> Result<RuntimeSubmitOutcomeV1, GraphDispatchError> {
        let writer = self
            .lock_state()
            .writer
            .clone()
            .ok_or(GraphDispatchError::Closed)?;
        writer
            .submit(request, probe)
            .await
            .map_err(|error| GraphDispatchError::Writer(error.to_string()))
    }

    pub fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, GraphDispatchError> {
        let readers = self
            .lock_state()
            .readers
            .clone()
            .ok_or(GraphDispatchError::Closed)?;
        let mut reader = readers
            .acquire(&request, probe, Duration::ZERO)
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
        if let Some(writer) = &state.writer {
            writer.begin_drain();
        }
        if let Some(readers) = &state.readers {
            readers.begin_drain();
        }
        state.admission_open = false;
        Ok(())
    }

    pub fn close_and_join(&self) -> Result<(), String> {
        let (writer, readers) = {
            let mut state = self.lock_state();
            if state.closed {
                return Ok(());
            }
            if state.admission_open {
                return Err("graph physical attachment must drain before close".to_owned());
            }
            let leased_readers = state.readers.as_ref().map_or(0, |readers| {
                let snapshot = readers.snapshot();
                u32::from(snapshot.leased_general) + u32::from(snapshot.leased_health)
            });
            let queued = state
                .writer
                .as_ref()
                .map(|writer| writer.telemetry_snapshot())
                .map_or(0, |snapshot| snapshot.queue.queued_operations);
            if leased_readers != 0 || queued != 0 {
                return Err(format!(
                    "graph physical attachment still has {leased_readers} readers and {queued} queued writes"
                ));
            }
            state.closed = true;
            (state.writer.take(), state.readers.take())
        };
        drop(readers);
        if let Some(writer) = writer {
            let writer = Arc::try_unwrap(writer)
                .map_err(|_| "graph writer is still serving a request".to_owned())?;
            writer
                .shutdown_and_join()
                .map_err(|error| format!("join graph writer: {error}"))?;
        }
        Ok(())
    }

    fn lock_state(&self) -> MutexGuard<'_, GraphRuntimePhysicalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    Prepare(GraphPhysicalAttachmentPrepareError),
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for GraphPhysicalAttachmentStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(error) => write!(formatter, "prepare graph attachment: {error}"),
            Self::Reader(error) => write!(formatter, "start graph readers: {error}"),
            Self::Writer(error) => write!(formatter, "start graph writer: {error}"),
        }
    }
}

impl Error for GraphPhysicalAttachmentStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Prepare(error) => Some(error),
            Self::Reader(error) => Some(error),
            Self::Writer(error) => Some(error),
        }
    }
}
