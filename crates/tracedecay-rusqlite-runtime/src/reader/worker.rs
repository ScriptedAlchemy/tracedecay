use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, InterruptHandle, Transaction, TransactionBehavior};
use tracedecay_domain::process_heap::{
    IDLE_THREAD_COLLECTION_WAIT_V1, collect_idle_thread_heap_v1,
};
use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1,
    StorageRuntimeErrorV1, UnavailableReasonV1,
};

use crate::connection::{self, ConnectionMode, OpenedDatabaseFile};
use crate::exact_sql::{
    ExactSqlError, ExactSqlRows, ExactSqlStatement, ExactSqlStatementBudget, execute_query,
    read_across_wal_recovery,
};
use crate::telemetry::{ReaderAdmissionRecorder, take_observed_vm};

use super::{ExistingReaderLocator, ReaderStartError};

const REPLY_POLL_QUANTUM: Duration = Duration::from_millis(5);

/// How often an idle snapshot checks whether the checkpoint fence revoked it.
const REVOCATION_POLL: Duration = Duration::from_millis(20);

/// The checkpoint fence's claim on one worker's snapshot.
///
/// A held snapshot keeps its WAL read slot, so the writer cannot restart the
/// log behind it. When a hard-pressure drain outlives the read deadline, the
/// writer revokes the snapshots older than it: any running statement is
/// interrupted, the worker rolls the read back, and the snapshot's later reads
/// fail as expired.
#[derive(Default)]
pub(crate) struct SnapshotRevocation {
    requested: AtomicBool,
    rolled_back: AtomicBool,
}

impl SnapshotRevocation {
    fn reset(&self) {
        self.requested.store(false, Ordering::Release);
        self.rolled_back.store(false, Ordering::Release);
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// The worker rolled the revoked snapshot back, so it no longer holds the
    /// WAL even while its lease is still out.
    pub(crate) fn rolled_back(&self) -> bool {
        self.rolled_back.load(Ordering::Acquire)
    }
}

/// What the checkpoint blockers keep for each open snapshot to revoke it.
#[derive(Clone)]
pub(crate) struct SnapshotRevoker {
    revocation: Arc<SnapshotRevocation>,
    interrupt: Arc<InterruptHandle>,
}

impl SnapshotRevoker {
    pub(crate) fn revoke(&self) {
        if !self.revocation.requested.swap(true, Ordering::AcqRel) {
            self.interrupt.interrupt();
        }
    }

    pub(crate) fn rolled_back(&self) -> bool {
        self.revocation.rolled_back()
    }
}

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
    /// The worker task has ended; its connection and cache are gone.
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
        lifetime: Duration,
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
        budget: ExactSqlStatementBudget,
        reply: SyncSender<Result<ExactSqlRows, ExactSqlError>>,
    },
    StoreSize {
        reply: SyncSender<Result<StoreSizeTelemetrySample, ReaderWorkerError>>,
    },
    TableSizes {
        reply: SyncSender<Result<Vec<TableSizeTelemetrySample>, ReaderWorkerError>>,
    },
    End {
        reply: async_channel::Sender<Result<(), ReaderWorkerError>>,
    },
    ReleaseMemory {
        reply: SyncSender<Result<(), ReaderWorkerError>>,
    },
    Shutdown,
}

pub(crate) type SnapshotEndReply = async_channel::Receiver<Result<(), ReaderWorkerError>>;

#[derive(Clone)]
pub(crate) struct WorkerClient {
    sender: async_channel::Sender<WorkerCommand>,
    snapshot_sender: Arc<Mutex<Option<async_channel::Sender<SnapshotCommand>>>>,
    /// Set while the worker runs its snapshot loop, from `Begin` until the
    /// loop returns, which can outlast the snapshot sender `begin_end` takes.
    in_snapshot: Arc<AtomicBool>,
    interrupt: Arc<InterruptHandle>,
    revocation: Arc<SnapshotRevocation>,
}

pub(crate) struct SpawnedWorker {
    pub client: WorkerClient,
    pub join: crate::storage_pool::PoolTask,
    pub opened_file_identity: u64,
}

impl WorkerClient {
    pub(crate) fn revoker(&self) -> SnapshotRevoker {
        SnapshotRevoker {
            revocation: Arc::clone(&self.revocation),
            interrupt: Arc::clone(&self.interrupt),
        }
    }

    /// Opens a snapshot that ends itself `lifetime` after it began: the worker
    /// rolls it back and its later reads fail as expired.
    pub fn begin(&self, lifetime: Duration) -> Result<(), ReaderWorkerError> {
        if crate::storage_pool::on_pool() {
            return Err(ReaderWorkerError::WorkerClosed);
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.sender
            .try_send(WorkerCommand::Begin { lifetime, reply })
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
        sender
            .try_send(SnapshotCommand::Pin { reply })
            .map_err(|_| {
                ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
            })?;
        receive
            .recv()
            .map_err(|_| {
                ExactSqlError::ReaderUnavailable(ReaderWorkerError::WorkerClosed.to_string())
            })?
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))
    }

    #[track_caller]
    pub fn execute(
        &self,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RuntimeReadOutcomeV1, ReaderWorkerError> {
        crate::exact_sql::note_native_thread_reader_wait();
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .try_send(SnapshotCommand::Execute {
                request: Box::new(request),
                reply,
            })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        self.receive_with_probe(receive, probe)
    }

    pub fn execute_exact_sql_query(
        &self,
        request: ExactSqlStatement,
        budget: ExactSqlStatementBudget,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        let sender = self
            .snapshot_sender()
            .map_err(|error| ExactSqlError::ReaderUnavailable(error.to_string()))?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .try_send(SnapshotCommand::ExactSqlQuery {
                request,
                budget,
                reply,
            })
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
            .try_send(SnapshotCommand::StoreSize { reply })
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
            .try_send(SnapshotCommand::TableSizes { reply })
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
        if crate::storage_pool::on_pool() {
            return Err(ReaderWorkerError::WorkerClosed);
        }
        if let Ok(sender) = self.snapshot_sender() {
            let (reply, receive) = mpsc::sync_channel(1);
            if sender
                .try_send(SnapshotCommand::ReleaseMemory { reply })
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
            .try_send(WorkerCommand::ReleaseMemory { reply })
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

    pub fn begin_end(&self) -> Result<SnapshotEndReply, ReaderWorkerError> {
        let sender = self
            .snapshot_sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(ReaderWorkerError::SnapshotNotActive)?;
        let (reply, receive) = async_channel::bounded(1);
        sender
            .try_send(SnapshotCommand::End { reply })
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
            let _ = sender.try_send(SnapshotCommand::Shutdown);
        } else {
            let _ = self.sender.try_send(WorkerCommand::Shutdown);
        }
    }

    fn snapshot_sender(&self) -> Result<async_channel::Sender<SnapshotCommand>, ReaderWorkerError> {
        if crate::storage_pool::on_pool() {
            return Err(ReaderWorkerError::WorkerClosed);
        }
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
    if crate::storage_pool::on_pool() {
        return Err(ReaderStartError::RuntimeUnavailable);
    }
    let worker_open_path = locator.worker_open_path()?;
    let (sender, receiver) = async_channel::unbounded();
    let receiver = crate::storage_pool::CommandReceiver::new(receiver);
    let snapshot_sender = Arc::new(Mutex::new(None));
    let worker_snapshot_sender = Arc::clone(&snapshot_sender);
    let in_snapshot = Arc::new(AtomicBool::new(false));
    let worker_in_snapshot = Arc::clone(&in_snapshot);
    let revocation = Arc::new(SnapshotRevocation::default());
    let worker_revocation = Arc::clone(&revocation);
    let (started, startup) = mpsc::sync_channel(1);
    let join = crate::storage_pool::spawn(async move {
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
        // Reading the header cookie opens that read transaction without
        // parsing the schema, which an idle reader would never use: on a
        // store with hundreds of schema objects the parse dominates startup.
        if let Err(error) =
            connection.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))
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
            &worker_revocation,
            &mut executor,
            admission,
        )
        .await;
    })
    .map_err(|_| ReaderStartError::RuntimeUnavailable)?;
    let (interrupt, opened_file_identity) = startup
        .recv()
        .map_err(|_| ReaderStartError::StartupChannelClosed)??;
    Ok(SpawnedWorker {
        client: WorkerClient {
            sender,
            snapshot_sender,
            in_snapshot,
            interrupt,
            revocation,
        },
        join,
        opened_file_identity,
    })
}

async fn run<E: ReaderQueryExecutor>(
    mut connection: Connection,
    receiver: crate::storage_pool::CommandReceiver<WorkerCommand>,
    published: Arc<Mutex<Option<async_channel::Sender<SnapshotCommand>>>>,
    in_snapshot: &AtomicBool,
    revocation: &SnapshotRevocation,
    executor: &mut E,
    admission: Arc<ReaderAdmissionRecorder>,
) {
    while let Some(command) = next_command(&receiver).await {
        match command {
            WorkerCommand::Shutdown => break,
            WorkerCommand::ReleaseMemory { reply } => {
                let _ = reply.send(shrink_connection_memory(&connection));
            }
            WorkerCommand::Begin { lifetime, reply } => {
                revocation.reset();
                let expires = Instant::now() + lifetime;
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Deferred)
                    .map_err(|error| {
                        ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                            operation: format!("begin deferred reader snapshot: {error}"),
                        })
                    })
                    // SAFETY: the transaction borrows `connection` by `&mut`,
                    // and this task owns that connection.
                    .map(|transaction| unsafe {
                        crate::storage_pool::TaskTransaction::new(transaction)
                    });
                match transaction {
                    Ok(transaction) => {
                        let (sender, commands) = async_channel::unbounded();
                        in_snapshot.store(true, Ordering::Release);
                        *published
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
                        if reply.send(Ok(())).is_err() {
                            return;
                        }
                        let mut end = SnapshotEnd {
                            revocation,
                            expires,
                            idle_limit: crate::reader::READ_SNAPSHOT_IDLE_LIMIT,
                            last_active: std::cell::Cell::new(Instant::now()),
                        };
                        if run_snapshot(
                            transaction,
                            crate::storage_pool::CommandReceiver::new(commands),
                            executor,
                            &admission,
                            &mut end,
                        )
                        .await
                        {
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

async fn next_command(
    receiver: &crate::storage_pool::CommandReceiver<WorkerCommand>,
) -> Option<WorkerCommand> {
    loop {
        collect_idle_thread_heap_v1();
        match crate::storage_pool::timeout(IDLE_THREAD_COLLECTION_WAIT_V1, receiver.recv()).await {
            Some(Ok(command)) => return Some(command),
            Some(Err(_)) => return None,
            None => {}
        }
    }
}

async fn run_snapshot<E: ReaderQueryExecutor>(
    transaction: crate::storage_pool::TaskTransaction<'_>,
    commands: crate::storage_pool::CommandReceiver<SnapshotCommand>,
    executor: &mut E,
    admission: &ReaderAdmissionRecorder,
    end: &mut SnapshotEnd<'_>,
) -> bool {
    let mut active = Some(transaction);
    loop {
        let command = match crate::storage_pool::timeout(REVOCATION_POLL, commands.recv()).await {
            Some(Ok(command)) => command,
            None => {
                end.end_if_due(&mut active);
                continue;
            }
            Some(Err(_)) => return false,
        };
        end.end_if_due(&mut active);
        let command = match command {
            SnapshotCommand::End { reply } => {
                let result = active.take().map_or(Ok(()), |transaction| {
                    transaction.into_inner().rollback().map_err(|error| {
                        ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                            operation: format!("close reader snapshot: {error}"),
                        })
                    })
                });
                let _ = reply.try_send(result);
                return false;
            }
            SnapshotCommand::Shutdown => return true,
            command => command,
        };
        let Some(transaction) = active.as_ref() else {
            reject_revoked(command);
            continue;
        };
        match command {
            SnapshotCommand::Pin { reply } => {
                // Any read starts the snapshot; counting the schema would
                // read every schema page on each pinned snapshot.
                let result = transaction
                    .prepare_cached("SELECT EXISTS(SELECT 1 FROM sqlite_schema)")
                    .and_then(|mut statement| statement.query_row([], |row| row.get::<_, i64>(0)))
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
                            .execute_read(transaction, &request)
                            .map_err(ReaderWorkerError::Storage)
                    }
                };
                admission.executed(started.elapsed(), take_observed_vm());
                let _ = reply.send(result);
            }
            SnapshotCommand::ExactSqlQuery {
                request,
                budget,
                reply,
            } => {
                let started = Instant::now();
                let budget = end.clamp(budget);
                let active = &mut active;
                let result = read_across_wal_recovery(move || {
                    let transaction = active.as_ref().expect("only this task ends the snapshot");
                    let _span = tracing::trace_span!("rusqlite.reader.exact_sql").entered();
                    execute_query(transaction, request.clone(), budget)
                })
                .await;
                admission.executed(started.elapsed(), take_observed_vm());
                let _ = reply.send(result);
            }
            SnapshotCommand::StoreSize { reply } => {
                let read = || -> Result<StoreSizeTelemetrySample, rusqlite::Error> {
                    let page_size = transaction
                        .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))?;
                    let page_count = transaction
                        .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))?;
                    let freelist_pages =
                        transaction.pragma_query_value(None, "freelist_count", |row| {
                            row.get::<_, i64>(0)
                        })?;
                    let page_size_bytes = u32::try_from(page_size)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, page_size))?;
                    let page_count = u64::try_from(page_count)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, page_count))?;
                    let freelist_pages = u64::try_from(freelist_pages)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, freelist_pages))?;
                    Ok(StoreSizeTelemetrySample {
                        page_size_bytes,
                        page_count,
                        freelist_pages,
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
                let read = || -> Result<Vec<TableSizeTelemetrySample>, rusqlite::Error> {
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
                                    rusqlite::Error::IntegralValueOutOfRange(1, bytes)
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
                let _ = reply.send(shrink_connection_memory(transaction));
            }
            SnapshotCommand::End { .. } | SnapshotCommand::Shutdown => {}
        }
        end.touch();
    }
}

/// When a held snapshot must end before its lease does: the checkpoint fence
/// revoked it, it sat idle too long, or its lifetime ran out.
struct SnapshotEnd<'a> {
    revocation: &'a SnapshotRevocation,
    expires: Instant,
    idle_limit: Duration,
    last_active: std::cell::Cell<Instant>,
}

impl SnapshotEnd<'_> {
    /// Revoked, past its lifetime, or idle past the idle limit.
    fn due(&self, now: Instant) -> bool {
        self.revocation.requested()
            || now >= self.expires
            || now >= self.last_active.get() + self.idle_limit
    }

    /// A command finished: the snapshot is in use, so its idle clock restarts.
    fn touch(&self) {
        self.last_active.set(Instant::now());
    }

    /// Rolls the snapshot back once it is due, so its WAL read slot is free
    /// while its lease is still out.
    fn end_if_due(&self, active: &mut Option<crate::storage_pool::TaskTransaction<'_>>) {
        if self.due(Instant::now())
            && let Some(transaction) = active.take()
        {
            // A failed rollback drops the transaction, which rolls back again.
            let _ = transaction.into_inner().rollback();
            self.revocation.rolled_back.store(true, Ordering::Release);
        }
    }

    /// A statement on this snapshot stops at the snapshot's end as well as at
    /// its own budget.
    fn clamp(&self, budget: ExactSqlStatementBudget) -> ExactSqlStatementBudget {
        let until = match budget {
            ExactSqlStatementBudget::Until(deadline) => deadline,
            ExactSqlStatementBudget::Request => {
                Instant::now() + crate::exact_sql::EXACT_SQL_EXECUTION_LIMIT
            }
            ExactSqlStatementBudget::Convergence => self.expires,
        };
        ExactSqlStatementBudget::Until(until.min(self.expires))
    }
}

/// Answers a command that reached a revoked snapshot.
fn reject_revoked(command: SnapshotCommand) {
    let expired = || ReaderWorkerError::Interrupted {
        reason: UnavailableReasonV1::SnapshotExpired,
    };
    match command {
        SnapshotCommand::Pin { reply } => {
            let _ = reply.send(Err(expired()));
        }
        SnapshotCommand::Execute { reply, .. } => {
            let _ = reply.send(Err(expired()));
        }
        SnapshotCommand::ExactSqlQuery { reply, .. } => {
            let _ = reply.send(Err(ExactSqlError::ReaderUnavailable(expired().to_string())));
        }
        SnapshotCommand::StoreSize { reply } => {
            let _ = reply.send(Err(expired()));
        }
        SnapshotCommand::TableSizes { reply } => {
            let _ = reply.send(Err(expired()));
        }
        SnapshotCommand::ReleaseMemory { reply } => {
            let _ = reply.send(Ok(()));
        }
        SnapshotCommand::End { .. } | SnapshotCommand::Shutdown => {}
    }
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
mod snapshot_end_tests {
    use std::time::{Duration, Instant};

    use super::{SnapshotEnd, SnapshotRevocation};

    /// A snapshot that keeps issuing statements stays open up to its
    /// lifetime; one that sits idle past the idle limit ends, and so does a
    /// revoked one.
    #[test]
    fn a_snapshot_ends_when_idle_too_long_revoked_or_past_its_lifetime() {
        let start = Instant::now();
        let revocation = SnapshotRevocation::default();
        let end = SnapshotEnd {
            revocation: &revocation,
            expires: start + Duration::from_hours(1),
            idle_limit: Duration::from_secs(30),
            last_active: std::cell::Cell::new(start),
        };
        assert!(!end.due(start + Duration::from_secs(29)));
        assert!(
            end.due(start + Duration::from_secs(30)),
            "idle past the limit"
        );

        end.last_active.set(start + Duration::from_mins(50));
        assert!(
            !end.due(start + Duration::from_mins(50) + Duration::from_secs(29)),
            "a busy snapshot is not idle however long it has been open"
        );
        assert!(
            end.due(start + Duration::from_hours(1)),
            "past its lifetime"
        );

        end.last_active.set(start);
        revocation
            .requested
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(end.due(start), "revoked");
    }
}

#[cfg(test)]
mod release_memory_tests {
    use std::sync::mpsc::Receiver;
    use std::thread::{self, JoinHandle};
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
        spawned
            .client
            .begin(crate::reader::READ_SNAPSHOT_LIFETIME)
            .unwrap();
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
            .try_send(WorkerCommand::Begin {
                lifetime: crate::reader::READ_SNAPSHOT_LIFETIME,
                reply: begin,
            })
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
