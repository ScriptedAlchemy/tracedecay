use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rusqlite::{Connection, InterruptHandle, Transaction, TransactionBehavior};
use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1,
    StorageRuntimeErrorV1, UnavailableReasonV1,
};

use crate::connection::{self, ConnectionMode, OpenedDatabaseFile};
use crate::exact_sql::{ExactSqlError, ExactSqlRows, ExactSqlStatement, execute_query};

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
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct WorkerClient {
    sender: Sender<WorkerCommand>,
    snapshot_sender: Arc<Mutex<Option<Sender<SnapshotCommand>>>>,
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

    pub fn store_size(&self) -> Result<StoreSizeTelemetrySample, ReaderWorkerError> {
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::StoreSize { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        receive
            .recv()
            .map_err(|_| ReaderWorkerError::WorkerClosed)?
    }

    pub fn table_sizes(&self) -> Result<Vec<TableSizeTelemetrySample>, ReaderWorkerError> {
        let sender = self.snapshot_sender()?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .send(SnapshotCommand::TableSizes { reply })
            .map_err(|_| ReaderWorkerError::WorkerClosed)?;
        receive
            .recv()
            .map_err(|_| ReaderWorkerError::WorkerClosed)?
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
) -> Result<SpawnedWorker, ReaderStartError> {
    let worker_open_path = locator.worker_open_path()?;
    let (sender, receiver) = mpsc::channel();
    let snapshot_sender = Arc::new(Mutex::new(None));
    let worker_snapshot_sender = Arc::clone(&snapshot_sender);
    let (started, startup) = mpsc::sync_channel(1);
    let join = thread::Builder::new()
        .name("tracedecay-rusqlite-reader".to_owned())
        .spawn(move || {
            let connection = match connection::open(&worker_open_path, ConnectionMode::Reader) {
                Ok(connection) => connection,
                Err(error) if error.is_open_failure() => {
                    let _ = started.send(Err(ReaderStartError::OpenFailed));
                    return;
                }
                Err(_) => {
                    let _ = started.send(Err(ReaderStartError::ReadOnlySetupFailed));
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
            let _keep_pinned_database_alive = locator;
            let interrupt = Arc::new(connection.get_interrupt_handle());
            if started
                .send(Ok((Arc::clone(&interrupt), opened_file_identity)))
                .is_err()
            {
                return;
            }
            run(connection, receiver, worker_snapshot_sender, &mut executor);
        })
        .map_err(ReaderStartError::ThreadSpawn)?;
    let (interrupt, opened_file_identity) = startup
        .recv()
        .map_err(|_| ReaderStartError::StartupChannelClosed)??;
    Ok(SpawnedWorker {
        client: WorkerClient {
            sender,
            snapshot_sender,
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
    executor: &mut E,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            WorkerCommand::Shutdown => break,
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
                        *published
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
                        if reply.send(Ok(())).is_err() {
                            return;
                        }
                        if run_snapshot(transaction, commands, executor) {
                            return;
                        }
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

fn run_snapshot<E: ReaderQueryExecutor>(
    transaction: Transaction<'_>,
    commands: Receiver<SnapshotCommand>,
    executor: &mut E,
) -> bool {
    while let Ok(command) = commands.recv() {
        match command {
            SnapshotCommand::Pin { reply } => {
                let result = transaction
                    .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
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
                let result = executor
                    .execute_read(&transaction, &request)
                    .map_err(ReaderWorkerError::Storage);
                let _ = reply.send(result);
            }
            SnapshotCommand::ExactSqlQuery { request, reply } => {
                let _ = reply.send(execute_query(&transaction, request));
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
                    let mut statement = transaction.prepare(
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

fn interruption(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    probe.interruption().map(|interruption| match interruption {
        RuntimeInterruptionV1::Cancelled => UnavailableReasonV1::Cancelled,
        RuntimeInterruptionV1::DeadlineExceeded => UnavailableReasonV1::DeadlineExceeded,
    })
}
