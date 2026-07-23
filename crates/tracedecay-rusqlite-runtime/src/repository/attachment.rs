use std::{
    error::Error,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::Transaction;
use tracedecay_store::{
    AdmissionConfigV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, WriterStartError, WriterState,
    reader::{
        ExistingReaderLocator, ReaderAcquireError, ReaderPool, ReaderQueryExecutor,
        ReaderStartError,
    },
};

use super::{ConcreteRepositoryReadExecutor, ConcreteRepositoryWriteExecutor};

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
        if matches!(binding.shard_id.scope, StoreShardScopeV1::Code { .. }) {
            return Err(RepositoryAttachmentStartError::UnsupportedShardScope);
        }
        let writer_locator =
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone())
                .map_err(RepositoryAttachmentStartError::Writer)?;
        let reader_locator = ExistingReaderLocator::new(binding.clone(), locator, path.clone())
            .map_err(RepositoryAttachmentStartError::Reader)?;
        let writer = PersistentWriter::start(
            writer_locator,
            admission.clone(),
            ConcreteRepositoryWriteExecutor::default(),
        )
        .map_err(RepositoryAttachmentStartError::Writer)?;
        let readers = match ReaderPool::start(
            reader_locator,
            admission.readers,
            RepositoryRuntimeReadExecutor::default(),
        ) {
            Ok(readers) => readers,
            Err(error) => {
                let _ = writer.shutdown_and_join();
                return Err(RepositoryAttachmentStartError::Reader(error));
            }
        };
        Ok(RepositoryRuntimePhysicalAttachment {
            state: Mutex::new(RepositoryRuntimePhysicalState {
                binding,
                database_path: path,
                writer: Some(Arc::new(writer)),
                readers: Some(readers),
                admission_open: true,
                closed: false,
            }),
        })
    }
}

#[derive(Debug)]
pub enum RepositoryAttachmentStartError {
    UnsupportedShardScope,
    Reader(ReaderStartError),
    Writer(WriterStartError),
}

impl fmt::Display for RepositoryAttachmentStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedShardScope => {
                formatter.write_str("repository attachment does not own code shards")
            }
            Self::Reader(error) => write!(formatter, "start repository readers: {error}"),
            Self::Writer(error) => write!(formatter, "start repository writer: {error}"),
        }
    }
}

impl Error for RepositoryAttachmentStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
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
    writer: Option<Arc<PersistentWriter>>,
    readers: Option<ReaderPool<RepositoryRuntimeReadExecutor>>,
    admission_open: bool,
    closed: bool,
}

impl RepositoryRuntimePhysicalAttachment {
    pub fn binding(&self) -> StoreRuntimeBindingV1 {
        self.lock_state().binding.clone()
    }

    pub fn snapshot(&self) -> RepositoryRuntimePhysicalSnapshot {
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
        RepositoryRuntimePhysicalSnapshot {
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
    ) -> Result<RuntimeSubmitOutcomeV1, RepositoryDispatchError> {
        let writer = self
            .lock_state()
            .writer
            .clone()
            .ok_or(RepositoryDispatchError::Closed)?;
        writer
            .submit(request, probe)
            .await
            .map_err(|error| RepositoryDispatchError::Writer(error.to_string()))
    }

    pub fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, RepositoryDispatchError> {
        let readers = self
            .lock_state()
            .readers
            .clone()
            .ok_or(RepositoryDispatchError::Closed)?;
        let mut reader = readers
            .acquire(&request, probe, Duration::ZERO)
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
                return Err("repository physical attachment must drain before close".to_owned());
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
                    "repository physical attachment still has {leased_readers} readers and {queued} queued writes"
                ));
            }
            state.closed = true;
            (state.writer.take(), state.readers.take())
        };
        drop(readers);
        if let Some(writer) = writer {
            let writer = Arc::try_unwrap(writer)
                .map_err(|_| "repository writer is still serving a request".to_owned())?;
            writer
                .shutdown_and_join()
                .map_err(|error| format!("join repository writer: {error}"))?;
        }
        Ok(())
    }

    fn lock_state(&self) -> MutexGuard<'_, RepositoryRuntimePhysicalState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        RuntimeReadOutcomeV1::new(
            Some(value),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
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
