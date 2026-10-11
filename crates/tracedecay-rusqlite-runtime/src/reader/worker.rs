use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tracedecay_domain::process_heap::{
    IDLE_THREAD_COLLECTION_WAIT_V1, collect_idle_thread_heap_v1,
};
use tracedecay_sql::{
    Connection, ConnectionControl, InterruptHandle, Transaction, TransactionBehavior,
};
use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1,
    StorageRuntimeErrorV1, UnavailableReasonV1,
};

use crate::connection::{self, ConnectionMode, OpenedDatabaseFile};
use crate::exact_sql::{ExactSqlError, ExactSqlRows, ExactSqlStatement, execute_query};
use crate::telemetry::{ReaderAdmissionRecorder, take_observed_vm};

use super::{ExistingReaderLocator, ReaderStartError};

const REPLY_POLL_QUANTUM: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreSizeTelemetrySample {
    pub page_size_bytes: u32,
    pub page_count: u64,
    pub freelist_pages: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSizeTelemetrySample {
    pub table_name: String,
    pub bytes: u64,
}

/// Closed typed query seam. Implementations may map the existing read-operation
/// enum to owned SQL, but callers can never inject SQL, paths, pragmas, writes,
/// migrations, repair work, or arbitrary callbacks through this interface.
pub trait ReaderQueryExecutor: Clone + Send + Sync + 'static {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1>;
}

/// Reader executor for isolated runtimes that expose only the bounded exact-SQL
/// metadata adapters. Product-domain reads stay unavailable; exact-SQL
/// snapshots continue to use the same registered reader workers.
#[derive(Clone, Default)]
pub struct ExactSqlOnlyReaderV1;

impl ReaderQueryExecutor for ExactSqlOnlyReaderV1 {
    fn execute_read(
        &mut self,
        _snapshot: &Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        Err(StorageRuntimeErrorV1::Infrastructure {
            operation: "isolated exact-SQL runtime received a product-domain read".to_owned(),
        })
    }
}

/// Outcome of one worker's best-effort cache release.
///
/// Only a real per-connection fault (the pragma reporting a SQLite error) is
/// an `Err`. A worker that cannot be reached right now, terminated, or busy
/// inside a retained snapshot, has nothing this release can act on, and
/// reporting it as a failure would mask genuine storage errors behind
/// lifecycle noise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerMemoryRelease {
    Released,
    /// The worker entered a retained snapshot before it read the release;
    /// the queued command still shrinks the connection when the snapshot
    /// ends.
    SnapshotBusy,
    /// The worker thread has terminated; its connection and cache are gone.
    Closed,
}

#[derive(Debug)]
pub enum ReaderWorkerError {
    WorkerClosed,
    SnapshotAlreadyActive,
    SnapshotNotActive,
    Interrupted { reason: UnavailableReasonV1 },
    Storage(StorageRuntimeErrorV1),
}

impl fmt::Display for ReaderWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerClosed => f.write_str("SQLite reader worker closed"),
            Self::SnapshotAlreadyActive => f.write_str("reader snapshot is already active"),
            Self::SnapshotNotActive => f.write_str("reader snapshot is not active"),
            Self::Interrupted { reason } => {
                write!(f, "SQLite reader query interrupted: {reason:?}")
            }
            Self::Storage(error) => write!(f, "SQLite reader failed: {error}"),
        }
    }
}

impl Error for ReaderWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

enum WorkerCommand {
    Begin {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    ReleaseMemory {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    Shutdown,
}

enum SnapshotCommand {
    Pin {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    Execute {
        request: Box<RuntimeReadRequestV1>,
        reply: SyncSender<Result<RuntimeReadOutcomeV1, ReaderWorkerError>>,
    },
    ExactSqlQuery {
        request: ExactSqlStatement,
        reply: SyncSender<Result<ExactSqlRows, ExactSqlError>>,
    },
    StoreSize {
        reply: SyncSender<Result<StoreSizeTelemetrySample, ReaderWorkerError>>,
    },
    TableSizes {
        reply: SyncSender<Result<Vec<TableSizeTelemetrySample>, ReaderWorkerError>>,
    },
    End {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    ReleaseMemory {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct WorkerClient {
    sender: Sender<WorkerCommand>,
    snapshot_sender: Arc<Mutex<Option<Sender<SnapshotCommand>>>>,
    /// Set while the worker runs its snapshot loop, from `Begin` until the
    /// loop returns, which can outlast the snapshot sender `begin_end` takes.
    in_snapshot: Arc<AtomicBool>,
    interrupt: Arc<InterruptHandle>,
}

pub(crate) struct SpawnedWorker {
    pub client: WorkerClient,
    pub join: JoinHandle<()>,
    pub opened_file_identity: u64,
}

impl WorkerClient {
    pub fn begin(&self) -> Result<(), ReaderWorkerError> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .send(WorkerCommand::Begin { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        receive
            .recv()
            .map_err(|_| ReaderWorkerError::WorkerClosed)??;
        if self
            .snapshot_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
        {
            return Err(ReaderWorkerError::WorkerClosed);
        }
        Ok(())
    }

    pub fn pin_exact_sql(&self) -> Result<(), ExactSqlError> {
        let sender = self
            .snapshot_sender()
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender.send(SnapshotCommand::Pin { reply }).map_err(|_| {
            ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
        })?;
        receive
            .recv()
            .map_err(|_| {
                ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
            })?
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))
    }

    pub fn execute(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderWorkerError> {
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::Execute {
                request: Box::new(request),
                reply,
            })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        self.receive_with_probe(receive, probe)
    }

    pub fn execute_exact_sql_query(
        &self,
        request: ExactSqlStatement,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        let sender = self
            .snapshot_sender()
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::ExactSqlQuery { request, reply })
            .map_err(|_| {
                ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
            })?;
        receive
            .recv()
            .map_err(|_| {
                ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
            })
            .and_then(std::convert::identity)
    }

    /// Store-size telemetry is a bounded metadata read (three pragmas); a
    /// worker that cannot answer within `reply_bound` is interrupted and
    /// reported unavailable instead of capturing the caller indefinitely.
    pub fn store_size(
        &self,
        reply_bound: Duration,
    ) -> Result<StoreSizeTelemetrySample, ReaderWorkerError> {
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::StoreSize { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        match receive.recv_timeout(reply_bound) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.interrupt.interrupt();
                Err(ReaderWorkerError::Interrupted {
                    reason: UnavailableReasonV1::DeadlineExceeded,
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(ReaderWorkerError::WorkerClosed),
        }
    }

    /// Table-size telemetry walks `dbstat` for every table, which is
    /// proportional to the whole store on disk; a worker that cannot answer
    /// within `reply_bound` is interrupted and reported unavailable instead
    /// of capturing the caller for the duration of a multi-gigabyte scan.
    pub fn table_sizes(
        &self,
        reply_bound: Duration,
    ) -> Result<Vec<TableSizeTelemetrySample>, ReaderWorkerError> {
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::TableSizes { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        match receive.recv_timeout(reply_bound) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.interrupt.interrupt();
                Err(ReaderWorkerError::Interrupted {
                    reason: UnavailableReasonV1::DeadlineExceeded,
                })
            }
            Err(RecvTimeoutError::Disconnected) => Err(ReaderWorkerError::WorkerClosed),
        }
    }

    /// Releases this worker's own SQLite page cache.
    ///
    /// `PRAGMA shrink_memory` is connection-local. A leased worker is already
    /// inside its snapshot loop, so the command has to travel that channel;
    /// an idle worker is waiting on the outer command channel.
    pub fn release_memory(&self) -> Result<WorkerMemoryRelease, ReaderWorkerError> {
        if let Ok(sender) = self.snapshot_sender() {
            let (reply, receive) = mpsc::sync_channel(1);
            if sender
                .send(SnapshotCommand::ReleaseMemory { reply })
                .is_ok()
                && let Ok(result) = receive.recv()
            {
                return result.map(|()| WorkerMemoryRelease::Released);
            }
            // The reply disconnecting does not mean the worker closed: `End`
            // returns from the snapshot loop without draining, dropping the
            // reply senders of queued commands, while the worker itself is
            // back on its outer command channel. Fall through and retry
            // there instead of reporting a live worker as closed.
        }
        let (reply, receive) = mpsc::sync_channel(1);
        if self
            .sender
            .send(WorkerCommand::ReleaseMemory { reply })
            .is_err()
        {
            return Ok(WorkerMemoryRelease::Closed);
        }
        // A slow reply is not a busy worker. A reader on its command loop
        // always comes back to this channel, however long the host stalls
        // it; only one inside its snapshot loop, behind a queued `Begin` or
        // still finishing an ended snapshot, can stay away indefinitely.
        loop {
            match receive.recv_timeout(REPLY_POLL_QUANTUM) {
                Ok(result) => return result.map(|()| WorkerMemoryRelease::Released),
                Err(RecvTimeoutError::Disconnected) => return Ok(WorkerMemoryRelease::Closed),
                Err(RecvTimeoutError::Timeout) if self.in_snapshot.load(Ordering::Acquire) => {
                    return Ok(WorkerMemoryRelease::SnapshotBusy);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    pub fn begin_end(&self) -> Result<Receiver<Result<(), ReaderWorkerError>>, ReaderWorkerError> {
        let sender = self
            .snapshot_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(ReaderWorkerError::SnapshotNotActive)?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::End { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        Ok(receive)
    }

    pub fn shutdown(&self) {
        self.interrupt.interrupt();
        if let Some(sender) = self
            .snapshot_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = sender.send(SnapshotCommand::Shutdown);
        } else {
            let _ = self.sender.send(WorkerCommand::Shutdown);
        }
    }

    fn snapshot_sender(&self) -> Result<Sender<SnapshotCommand>, ReaderWorkerError> {
        self.snapshot_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(ReaderWorkerError::SnapshotNotActive)
    }

    fn receive_with_probe<T>(
        &self,
        receive: Receiver<Result<T, ReaderWorkerError>>,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<T, ReaderWorkerError> {
        loop {
            if let Some(reason) = interruption(probe) {
                self.interrupt.interrupt();
                return Err(ReaderWorkerError::Interrupted { reason });
            }
            match receive.recv_timeout(REPLY_POLL_QUANTUM) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ReaderWorkerError::WorkerClosed);
                }
            }
        }
    }
}

pub(crate) fn spawn<E: ReaderQueryExecutor>(
    locator: ExistingReaderLocator,
    mut executor: E,
    admission: Arc<ReaderAdmissionRecorder>,
) -> Result<SpawnedWorker, ReaderStartError> {
    let worker_open_path = locator.worker_open_path()?;
    let (sender, receiver) = mpsc::channel();
    let snapshot_sender = Arc::new(Mutex::new(None));
    let worker_snapshot_sender = Arc::clone(&snapshot_sender);
    let in_snapshot = Arc::new(AtomicBool::new(false));
    let worker_in_snapshot = Arc::clone(&in_snapshot);
    let (started, startup) = mpsc::sync_channel(1);
    let join = thread::Builder::new()
        .name("tracedecay-rusqlite-reader".to_owned())
        .spawn(move || {
            let connection = match connection::open(&worker_open_path, ConnectionMode::Reader) {
                Ok(connection) => connection,
                Err(error) if error.is_open_failure() => {
                    let _ = started.send(Err(ReaderStartError::OpenFailed(error)));
                    return;
                }
                Err(error) => {
                    let _ = started.send(Err(ReaderStartError::ReadOnlySetupFailed(error)));
                    return;
                }
            };
            if let Err(error) = locator.verify_connection(&connection) {
                let _ = started.send(Err(error));
                return;
            }
            let opened_file_identity = match OpenedDatabaseFile::pin(&worker_open_path) {
                Ok(opened) => opened.identity(),
                Err(error) => {
                    let _ = started.send(Err(ReaderStartError::OpenedDatabaseIdentity(error)));
                    return;
                }
            };
            // Opening and policy verification do not enter the database schema.
            // Complete WAL-index recovery before this worker can race admitted writes.
            if let Err(error) =
                connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
                    row.get::<_, i64>(0)
                })
            {
                let _ = started.send(Err(ReaderStartError::SchemaProbeFailed(error)));
                return;
            }
            let _keep_pinned_database_alive = locator;
            let interrupt = Arc::new(connection.get_interrupt_handle());
            if started
                .send(Ok((Arc::clone(&interrupt), opened_file_identity)))
                .is_err()
            {
                return;
            }
            run(
                connection,
                receiver,
                worker_snapshot_sender,
                &worker_in_snapshot,
                &mut executor,
                admission,
            );
        })
        .map_err(ReaderStartError::ThreadSpawn)?;
    let (interrupt, opened_file_identity) = startup
        .recv()
        .map_err(|_| ReaderStartError::StartupChannelClosed)??;
    Ok(SpawnedWorker {
        client: WorkerClient {
            sender,
            snapshot_sender,
            in_snapshot,
            interrupt,
        },
        join,
        opened_file_identity,
    })
}

fn run<E: ReaderQueryExecutor>(
    mut connection: Connection,
    receiver: Receiver<WorkerCommand>,
    published: Arc<Mutex<Option<Sender<SnapshotCommand>>>>,
    in_snapshot: &AtomicBool,
    executor: &mut E,
    admission: Arc<ReaderAdmissionRecorder>,
) {
    while let Some(command) = next_command(&receiver) {
        match command {
            WorkerCommand::Shutdown => break,
            WorkerCommand::ReleaseMemory { reply } => {
                let _ = reply.send(shrink_connection_memory(&connection));
            }
            WorkerCommand::Begin { reply } => {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Deferred)
                    .map_err(|error| {
                        ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                            operation: format!("begin deferred reader snapshot: {error}"),
                        })
                    });
                match transaction {
                    Ok(transaction) => {
                        let (sender, commands) = mpsc::channel();
                        in_snapshot.store(true, Ordering::Release);
                        *published
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
                        if reply.send(Ok(())).is_err() {
                            return;
                        }
                        if run_snapshot(transaction, commands, executor, &admission) {
                            return;
                        }
                        // Clear the flag before the sender: a release that
                        // missed the sender queues on the outer channel and
                        // must not read a stale `in_snapshot` for a worker
                        // already back on its command loop.
                        in_snapshot.store(false, Ordering::Release);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
                *published
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
        }
    }
}

/// Wait for the next command, returning this thread's heap whenever the
/// sampler asked for it while the worker idles.
fn next_command(receiver: &Receiver<WorkerCommand>) -> Option<WorkerCommand> {
    loop {
        collect_idle_thread_heap_v1();
        match receiver.recv_timeout(IDLE_THREAD_COLLECTION_WAIT_V1) {
            Ok(command) => return Some(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn run_snapshot<E: ReaderQueryExecutor>(
    transaction: Transaction<'_>,
    commands: Receiver<SnapshotCommand>,
    executor: &mut E,
    admission: &ReaderAdmissionRecorder,
) -> bool {
    while let Ok(command) = commands.recv() {
        match command {
            SnapshotCommand::Pin { reply } => {
                // Any read starts the snapshot; counting the schema would
                // read every schema page on each pinned snapshot.
                let result = transaction
                    .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map(|_| ())
                    .map_err(|error| {
                        ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                            operation: format!("pin retained reader snapshot: {error}"),
                        })
                    });
                let _ = reply.send(result);
            }
            SnapshotCommand::Execute { request, reply } => {
                let started = Instant::now();
                let result = {
                    let _span = tracing::trace_span!("rusqlite.reader.execute").entered();
                    {
                        executor
                            .execute_read(&transaction, &request)
                            .map_err(ReaderWorkerError::Storage)
                    }
                };
                admission.executed(started.elapsed(), take_observed_vm());
                let _ = reply.send(result);
            }
            SnapshotCommand::ExactSqlQuery { request, reply } => {
                let started = Instant::now();
                let result = {
                    let _span = tracing::trace_span!("rusqlite.reader.exact_sql").entered();
                    execute_query(&transaction, request)
                };
                admission.executed(started.elapsed(), take_observed_vm());
                let _ = reply.send(result);
            }
            SnapshotCommand::StoreSize { reply } => {
                let read = || -> Result<StoreSizeTelemetrySample, String> {
                    let stats = transaction
                        .page_stats()
                        .map_err(|error| error.to_string())?;
                    Ok(StoreSizeTelemetrySample {
                        page_size_bytes: u32::try_from(stats.page_size)
                            .map_err(|_| format!("page size {} exceeds u32", stats.page_size))?,
                        page_count: stats.page_count,
                        freelist_pages: stats.freelist_count,
                    })
                };
                let result = read().map_err(|error| {
                    ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                        operation: format!("read store size telemetry: {error}"),
                    })
                });
                let _ = reply.send(result);
            }
            SnapshotCommand::TableSizes { reply } => {
                let read = || -> Result<Vec<TableSizeTelemetrySample>, tracedecay_sql::Error> {
                    let mut statement = transaction.prepare_cached(
                        "SELECT schema_entry.name, COALESCE(SUM(dbstat.payload), 0) \
                         FROM sqlite_schema AS schema_entry \
                         LEFT JOIN dbstat ON dbstat.name = schema_entry.name \
                         WHERE schema_entry.type = 'table' \
                           AND schema_entry.name NOT LIKE 'sqlite_%' \
                         GROUP BY schema_entry.name \
                         ORDER BY schema_entry.name",
                    )?;
                    statement
                        .query_map([], |row| {
                            let bytes = row.get::<_, i64>(1)?;
                            Ok(TableSizeTelemetrySample {
                                table_name: row.get(0)?,
                                bytes: u64::try_from(bytes).map_err(|_| {
                                    tracedecay_sql::Error::IntegralValueOutOfRange(1, bytes)
                                })?,
                            })
                        })?
                        .collect()
                };
                let result = read().map_err(|error| {
                    ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                        operation: format!("read table size telemetry: {error}"),
                    })
                });
                let _ = reply.send(result);
            }
            SnapshotCommand::ReleaseMemory { reply } => {
                let _ = reply.send(shrink_connection_memory(&transaction));
            }
            SnapshotCommand::End { reply } => {
                let result = transaction.rollback().map_err(|error| {
                    ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                        operation: format!("close reader snapshot: {error}"),
                    })
                });
                let _ = reply.send(result);
                return false;
            }
            SnapshotCommand::Shutdown => return true,
        }
    }
    false
}

fn shrink_connection_memory(connection: &Connection) -> Result<(), ReaderWorkerError> {
    connection
        .execute_batch("PRAGMA shrink_memory")
        .map_err(|error| {
            ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                operation: format!("release reader connection memory: {error}"),
            })
        })
}

fn interruption(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    probe.interruption().map(|interruption| match interruption {
        RuntimeInterruptionV1::Cancelled => UnavailableReasonV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded => UnavailableReasonV1::DeadlineExceeded,
    })
}

#[cfg(test)]
mod release_memory_tests {
    use std::time::Duration;

    use tracedecay_store::OperationPriorityV1;

    use super::*;
    use crate::reader::tests::{GateExecutor, Probe, TestStore, request};

    /// How long a test waits on one command reply before failing instead of
    /// hanging until Bazel's overall timeout.
    const REPLY_BOUND: Duration = Duration::from_secs(60);

    /// Parks the worker inside its snapshot loop on a read whose end is
    /// already queued: it holds no retained snapshot, yet it is not back on
    /// its outer command channel either, the state a deferred snapshot end
    /// leaves behind.
    fn finishing_snapshot(
        store: &TestStore,
        executor: &GateExecutor,
    ) -> (SpawnedWorker, JoinHandle<()>) {
        let spawned = spawn(
            store.locator(),
            executor.clone(),
            Arc::new(ReaderAdmissionRecorder::default()),
        )
        .unwrap();
        spawned.client.begin().unwrap();
        let reading = thread::spawn({
            let client = spawned.client.clone();
            let read = request(&store.binding, OperationPriorityV1::Foreground);
            move || {
                let probe = Probe::for_request(&read);
                let _ = client.execute(read, &probe);
            }
        });
        assert!(
            executor.entered.wait(REPLY_BOUND),
            "the read must be parked before its snapshot end is queued"
        );
        drop(spawned.client.begin_end().unwrap());
        (spawned, reading)
    }

    fn release_in_background(
        client: &WorkerClient,
    ) -> (
        Receiver<Result<WorkerMemoryRelease, ReaderWorkerError>>,
        JoinHandle<()>,
    ) {
        let (done, released) = mpsc::channel();
        let client = client.clone();
        let releasing = thread::spawn(move || done.send(client.release_memory()).unwrap());
        (released, releasing)
    }

    #[test]
    fn memory_release_skips_a_worker_still_finishing_its_snapshot() {
        let store = TestStore::new();
        let executor = GateExecutor::default();
        let (spawned, reading) = finishing_snapshot(&store, &executor);

        let (released, releasing) = release_in_background(&spawned.client);
        assert_eq!(
            released
                .recv_timeout(REPLY_BOUND)
                .expect("the release must answer while the read is parked")
                .unwrap(),
            WorkerMemoryRelease::SnapshotBusy,
            "the release must not wait out a read the worker is still running"
        );
        executor.release.set();

        releasing.join().unwrap();
        reading.join().unwrap();
        spawned.client.shutdown();
        spawned.join.join().unwrap();
    }

    #[test]
    fn memory_release_returns_while_the_worker_holds_a_retained_snapshot() {
        let store = TestStore::new();
        let executor = GateExecutor::default();
        let (spawned, reading) = finishing_snapshot(&store, &executor);
        let (begin, begun) = mpsc::sync_channel(1);
        spawned
            .client
            .sender
            .send(WorkerCommand::Begin { reply: begin })
            .unwrap();

        let (released, releasing) = release_in_background(&spawned.client);
        executor.release.set();
        begun
            .recv_timeout(REPLY_BOUND)
            .expect("the queued begin must answer")
            .unwrap();
        let outcome = released
            .recv_timeout(REPLY_BOUND)
            .expect("the release must answer")
            .unwrap();
        assert!(
            matches!(
                outcome,
                WorkerMemoryRelease::Released | WorkerMemoryRelease::SnapshotBusy
            ),
            "got {outcome:?}"
        );

        releasing.join().unwrap();
        reading.join().unwrap();
        spawned.client.shutdown();
        spawned.join.join().unwrap();
    }
}
