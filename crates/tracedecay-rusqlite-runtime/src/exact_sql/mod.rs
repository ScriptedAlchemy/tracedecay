//! Exact SQL transport between the runtime and its writer/reader pools.
//!
//! Authority comes only from an already-attached writer and reader pool. This
//! module exposes owned values, never a SQLite connection or filesystem path.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, TransactionBehavior, params_from_iter};
use tokio::sync::mpsc as tokio_mpsc;
use tracedecay_store::{
    OperationPriorityV1, StoreRuntimeBindingV1, UnavailableReasonV1, VerifiedStoreLocatorV1,
};

use crate::{
    PersistentWriter,
    reader::{
        ReaderAcquireError, ReaderPool, ReaderPoolSnapshot, ReaderQueryExecutor,
        StoreSizeTelemetrySample, TableSizeTelemetrySample,
    },
};

const MAX_QUERY_ROWS: usize = 10_000;
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 1024 * 1024;
const MAX_SQL_PARAMETERS: usize = 32_766;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXACT_SQL_ATTACHMENTS: i32 = 4;
const EXACT_SQL_PROGRESS_INTERVAL_OPS: i32 = 1_000;
#[cfg(not(test))]
const EXACT_SQL_EXECUTION_LIMIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const EXACT_SQL_EXECUTION_LIMIT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const EXACT_SQL_TRANSACTION_IDLE_LIMIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const EXACT_SQL_TRANSACTION_IDLE_LIMIT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const EXACT_SQL_TRANSACTION_LIMIT: Duration = Duration::from_secs(120);
#[cfg(test)]
const EXACT_SQL_TRANSACTION_LIMIT: Duration = Duration::from_millis(500);
const ROW_ALLOCATION_OVERHEAD: usize =
    std::mem::size_of::<ExactSqlRow>() + std::mem::size_of::<Vec<ExactSqlValue>>();
const CELL_ALLOCATION_OVERHEAD: usize = std::mem::size_of::<ExactSqlValue>();

mod command;
mod guard;
mod types;

pub use types::*;

pub(crate) use command::{WriterCommand, reject_writer_command, run_writer_command};

use command::TransactionCommand;
use guard::{AuthorizedDatabaseOperation, InsertTracker, with_exact_sql_guard};

type ExactSqlQuery = dyn Fn(ExactSqlStatement, OperationPriorityV1, Duration) -> Result<ExactSqlRows, ExactSqlError>
    + Send
    + Sync;
type ExactSqlSnapshotFactory = dyn Fn(OperationPriorityV1, Duration) -> Result<ExactSqlReadSnapshot, ExactSqlError>
    + Send
    + Sync;
type ExactSqlHealthSnapshotFactory =
    dyn Fn(Duration) -> Result<ExactSqlReadSnapshot, ExactSqlError> + Send + Sync;
type ReaderPoolOccupancyRead = dyn Fn() -> Option<ReaderPoolSnapshot> + Send + Sync;
type ExactSqlSnapshotQuery =
    dyn FnMut(ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> + Send;
type StoreSizeTelemetryRead = dyn Fn(
        Duration,
        &mut dyn FnMut() -> Option<UnavailableReasonV1>,
    ) -> Result<StoreSizeTelemetrySample, ReaderAcquireError>
    + Send
    + Sync;
type TableSizeTelemetryRead = dyn Fn(
        Duration,
        &mut dyn FnMut() -> Option<UnavailableReasonV1>,
    ) -> Result<Vec<TableSizeTelemetrySample>, ReaderAcquireError>
    + Send
    + Sync;

#[derive(Clone)]
pub struct ExactSqlHandle {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    writer: Option<tokio_mpsc::Sender<WriterCommand>>,
    query: Arc<ExactSqlQuery>,
    snapshot: Arc<ExactSqlSnapshotFactory>,
    health_snapshot: Arc<ExactSqlHealthSnapshotFactory>,
    store_size_telemetry: Arc<StoreSizeTelemetryRead>,
    table_size_telemetry: Arc<TableSizeTelemetryRead>,
    reader_pool_occupancy: Arc<ReaderPoolOccupancyRead>,
    last_insert_rowid: Arc<AtomicI64>,
    write_authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
}

impl ExactSqlHandle {
    pub fn attach<E: ReaderQueryExecutor>(
        writer: &PersistentWriter,
        readers: &ReaderPool<E>,
    ) -> Result<Self, ExactSqlError> {
        let paths_match = match (
            std::fs::canonicalize(writer.path()),
            std::fs::canonicalize(readers.path()),
        ) {
            (Ok(writer), Ok(reader)) => writer == reader,
            _ => false,
        };
        if writer.binding() != readers.binding()
            || writer.verified_locator() != readers.verified_locator()
            || !paths_match
        {
            return Err(ExactSqlError::AuthorityMismatch);
        }
        let sender = writer
            .exact_sql_sender()
            .ok_or(ExactSqlError::WriterUnavailable)?;
        Ok(Self::from_readers(
            writer.binding().clone(),
            writer.verified_locator().clone(),
            Some(sender),
            readers,
        ))
    }

    pub fn attach_read_only<E: ReaderQueryExecutor>(readers: &ReaderPool<E>) -> Self {
        Self::from_readers(
            readers.binding().clone(),
            readers.verified_locator().clone(),
            None,
            readers,
        )
    }

    fn from_readers<E: ReaderQueryExecutor>(
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        writer: Option<tokio_mpsc::Sender<WriterCommand>>,
        readers: &ReaderPool<E>,
    ) -> Self {
        let query_readers = readers.downgrade();
        let snapshot_readers = readers.downgrade();
        let health_snapshot_readers = readers.downgrade();
        let store_size_readers = readers.downgrade();
        let table_size_readers = readers.downgrade();
        let occupancy_readers = readers.downgrade();
        Self {
            binding,
            locator,
            writer,
            query: Arc::new(move |statement, priority, max_wait| {
                query_readers
                    .upgrade()
                    .ok_or_else(|| {
                        ExactSqlError::ReaderUnavailable(
                            "exact SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .execute_exact_sql_query(statement, priority, max_wait)
            }),
            snapshot: Arc::new(move |priority, max_wait| {
                snapshot_readers
                    .upgrade()
                    .ok_or_else(|| {
                        ExactSqlError::ReaderUnavailable(
                            "exact SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .begin_exact_sql_snapshot(priority, max_wait)
            }),
            health_snapshot: Arc::new(move |max_wait| {
                health_snapshot_readers
                    .upgrade()
                    .ok_or_else(|| {
                        ExactSqlError::ReaderUnavailable(
                            "exact SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .begin_exact_sql_health_snapshot(max_wait)
            }),
            store_size_telemetry: Arc::new(move |max_wait, interrupted| {
                store_size_readers
                    .upgrade()
                    .ok_or(ReaderAcquireError::Interrupted {
                        reason: UnavailableReasonV1::Draining,
                    })?
                    .read_store_size(max_wait, interrupted)
            }),
            table_size_telemetry: Arc::new(move |max_wait, interrupted| {
                table_size_readers
                    .upgrade()
                    .ok_or(ReaderAcquireError::Interrupted {
                        reason: UnavailableReasonV1::Draining,
                    })?
                    .read_table_sizes(max_wait, interrupted)
            }),
            reader_pool_occupancy: Arc::new(move || {
                occupancy_readers.upgrade().map(|pool| pool.snapshot())
            }),
            last_insert_rowid: Arc::new(AtomicI64::new(0)),
            write_authority: None,
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    pub fn read_only_clone(&self) -> Self {
        Self {
            binding: self.binding.clone(),
            locator: self.locator.clone(),
            writer: None,
            query: Arc::clone(&self.query),
            snapshot: Arc::clone(&self.snapshot),
            health_snapshot: Arc::clone(&self.health_snapshot),
            store_size_telemetry: Arc::clone(&self.store_size_telemetry),
            table_size_telemetry: Arc::clone(&self.table_size_telemetry),
            reader_pool_occupancy: Arc::clone(&self.reader_pool_occupancy),
            last_insert_rowid: Arc::clone(&self.last_insert_rowid),
            write_authority: None,
        }
    }

    pub fn with_write_authority(
        mut self,
        authority: Arc<dyn ExactSqlWriteAuthority>,
    ) -> Result<Self, ExactSqlError> {
        if self.writer.is_none() {
            return Err(ExactSqlError::WriterUnavailable);
        }
        self.write_authority = Some(authority);
        Ok(self)
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid.load(Ordering::Acquire)
    }

    /// Live reader-pool occupancy, or `None` once the pool has been closed.
    ///
    /// This takes no lease and runs no query: saturation has to stay
    /// observable precisely when no reader is available to answer with.
    pub fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot> {
        (self.reader_pool_occupancy)()
    }

    pub fn store_size_telemetry<F>(
        &self,
        reader_wait: Duration,
        mut interrupted: F,
    ) -> Result<StoreSizeTelemetrySample, ReaderAcquireError>
    where
        F: FnMut() -> Option<UnavailableReasonV1>,
    {
        (self.store_size_telemetry)(reader_wait, &mut interrupted)
    }

    pub fn table_size_telemetry<F>(
        &self,
        reader_wait: Duration,
        mut interrupted: F,
    ) -> Result<Vec<TableSizeTelemetrySample>, ReaderAcquireError>
    where
        F: FnMut() -> Option<UnavailableReasonV1>,
    {
        (self.table_size_telemetry)(reader_wait, &mut interrupted)
    }

    pub fn dispatch(
        &self,
        request: SqlRequest,
        reader_wait: Duration,
    ) -> Result<SqlResult, ExactSqlError> {
        match request {
            SqlRequest::Query(statement) => {
                self.query(statement, reader_wait).map(SqlResult::Queried)
            }
            request => self.dispatch_writer(request),
        }
    }

    pub fn execute(
        &self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlExecuteResult, ExactSqlError> {
        match self.dispatch_writer(SqlRequest::Execute(statement))? {
            SqlResult::Executed(result) => Ok(result),
            _ => Err(ExactSqlError::WriterUnavailable),
        }
    }

    pub fn validate(&self, statement: ExactSqlStatement) -> Result<(), ExactSqlError> {
        match self.dispatch_writer(SqlRequest::Validate(statement))? {
            SqlResult::Validated => Ok(()),
            _ => Err(ExactSqlError::WriterUnavailable),
        }
    }

    /// Interactive read. Admits against the whole general reader lane.
    pub fn query(
        &self,
        statement: ExactSqlStatement,
        max_wait: Duration,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        self.query_with_priority(statement, OperationPriorityV1::Foreground, max_wait)
    }

    /// Read under an explicit priority.
    ///
    /// Callers that know they are bulk or maintenance work pass `Background`
    /// so the reader pool keeps a slice of the general lane free for
    /// interactive reads.
    pub fn query_with_priority(
        &self,
        statement: ExactSqlStatement,
        priority: OperationPriorityV1,
        max_wait: Duration,
    ) -> Result<ExactSqlRows, ExactSqlError> {
        statement.validate()?;
        (self.query)(statement, priority, max_wait)
    }

    /// Checkpoints and truncates the WAL on the serialized writer connection.
    pub fn checkpoint_wal_truncate(&self) -> Result<ExactSqlRows, ExactSqlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(ExactSqlError::WriterUnavailable)?
            .try_send(WriterCommand::CheckpointWalTruncate {
                reply,
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| ExactSqlError::WriterUnavailable)?
    }

    pub fn execute_batch(&self, sql: String) -> Result<ExactSqlBatchResult, ExactSqlError> {
        validate_batch(&sql)?;
        match self.dispatch_writer(SqlRequest::ExecuteBatch(sql))? {
            SqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(ExactSqlError::WriterUnavailable),
        }
    }

    /// Enables incremental auto-vacuum through its fixed maintenance rebuild.
    pub fn repair_incremental_auto_vacuum(&self) -> Result<(), ExactSqlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(ExactSqlError::WriterUnavailable)?
            .try_send(WriterCommand::Vacuum {
                reply,
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| ExactSqlError::WriterUnavailable)?
    }

    /// Interactive read snapshot. Admits against the whole general lane.
    pub fn begin_read_snapshot(
        &self,
        max_wait: Duration,
    ) -> Result<ExactSqlReadSnapshot, ExactSqlError> {
        self.begin_read_snapshot_with_priority(OperationPriorityV1::Foreground, max_wait)
    }

    /// Read snapshot under an explicit priority. A pinned snapshot holds its
    /// worker for its whole lifetime, so declaring bulk work `Background` here
    /// matters more than for a one-shot query.
    pub fn begin_read_snapshot_with_priority(
        &self,
        priority: OperationPriorityV1,
        max_wait: Duration,
    ) -> Result<ExactSqlReadSnapshot, ExactSqlError> {
        (self.snapshot)(priority, max_wait)
    }

    pub fn begin_health_read_snapshot(
        &self,
        max_wait: Duration,
    ) -> Result<ExactSqlReadSnapshot, ExactSqlError> {
        (self.health_snapshot)(max_wait)
    }

    pub fn begin_immediate(&self) -> Result<ExactSqlTransaction, ExactSqlError> {
        self.begin_transaction(TransactionBehavior::Immediate, TransactionPolicy::Ordinary)
    }

    pub fn begin_deferred(&self) -> Result<ExactSqlTransaction, ExactSqlError> {
        self.begin_transaction(TransactionBehavior::Deferred, TransactionPolicy::Ordinary)
    }

    /// Begins the only transaction mode whose lease renews on progress.
    ///
    /// Reserved for schema installation and full-index bulk replacement — work
    /// that legitimately outlives one lease while continuously committing
    /// progress. The mode is intentionally not configurable: callers must
    /// attach a live write authority and opt into the long-lease transaction
    /// and revalidated-batch APIs. Shutdown, idleness, and authority revocation remain
    /// progress-handler cancellation conditions.
    pub fn begin_authorized_long_lease_immediate(
        &self,
    ) -> Result<ExactSqlTransaction, ExactSqlError> {
        if self.writer.is_none() {
            return Err(ExactSqlError::WriterUnavailable);
        }
        if self.write_authority.is_none() {
            return Err(ExactSqlError::AuthorityDenied(
                "long-lease transaction requires attached write authority".to_owned(),
            ));
        }
        self.begin_transaction(
            TransactionBehavior::Immediate,
            TransactionPolicy::AuthorizedLongLease,
        )
    }

    fn begin_transaction(
        &self,
        behavior: TransactionBehavior,
        policy: TransactionPolicy,
    ) -> Result<ExactSqlTransaction, ExactSqlError> {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (reply, response) = mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        self.writer
            .as_ref()
            .ok_or(ExactSqlError::WriterUnavailable)?
            .try_send(WriterCommand::BeginTransaction {
                behavior,
                policy,
                receiver,
                reply,
                last_insert_rowid: Arc::clone(&self.last_insert_rowid),
                expired: Arc::clone(&expired),
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| ExactSqlError::WriterUnavailable)??;
        Ok(ExactSqlTransaction {
            commands: Some(commands),
            expired,
            policy,
        })
    }

    fn dispatch_writer(&self, request: SqlRequest) -> Result<SqlResult, ExactSqlError> {
        validate_request(&request)?;
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(ExactSqlError::WriterUnavailable)?
            .try_send(WriterCommand::Dispatch {
                request,
                reply,
                last_insert_rowid: Arc::clone(&self.last_insert_rowid),
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| ExactSqlError::WriterUnavailable)?
    }
}

pub struct ExactSqlReadSnapshot {
    query: std::sync::Mutex<Box<ExactSqlSnapshotQuery>>,
}

impl ExactSqlReadSnapshot {
    pub(crate) fn new<F>(query: F) -> Self
    where
        F: FnMut(ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> + Send + 'static,
    {
        Self {
            query: std::sync::Mutex::new(Box::new(query)),
        }
    }

    pub fn query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> {
        statement.validate()?;
        self.query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)(statement)
    }
}

pub struct ExactSqlTransaction {
    commands: Option<mpsc::SyncSender<TransactionCommand>>,
    expired: Arc<AtomicBool>,
    policy: TransactionPolicy,
}

impl ExactSqlTransaction {
    pub fn attach_database(&self, attachment: ExactSqlAttachment) -> Result<(), ExactSqlError> {
        let sender = self
            .commands
            .as_ref()
            .ok_or(ExactSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Attach { attachment, reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    pub fn validate(&self, statement: ExactSqlStatement) -> Result<(), ExactSqlError> {
        match self.dispatch(SqlRequest::Validate(statement))? {
            SqlResult::Validated => Ok(()),
            _ => Err(ExactSqlError::TransactionClosed),
        }
    }

    pub fn execute(
        &self,
        statement: ExactSqlStatement,
    ) -> Result<ExactSqlExecuteResult, ExactSqlError> {
        match self.dispatch(SqlRequest::Execute(statement))? {
            SqlResult::Executed(result) => Ok(result),
            _ => Err(ExactSqlError::TransactionClosed),
        }
    }

    pub fn query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> {
        match self.dispatch(SqlRequest::Query(statement))? {
            SqlResult::Queried(result) => Ok(result),
            _ => Err(ExactSqlError::TransactionClosed),
        }
    }

    pub fn execute_batch(&self, sql: String) -> Result<ExactSqlBatchResult, ExactSqlError> {
        if sql.trim().is_empty() {
            return Err(ExactSqlError::InvalidStatement);
        }
        match self.dispatch(SqlRequest::ExecuteBatch(sql))? {
            SqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(ExactSqlError::TransactionClosed),
        }
    }

    /// Executes one batch with continuous authority revalidation.
    ///
    /// This is not a generic unbounded mode; it is accepted only by an
    /// authority-bound long-lease transaction. The writer actor re-verifies
    /// authority before, repeatedly during, and after execution.
    pub fn execute_authority_revalidated_batch(
        &self,
        sql: String,
    ) -> Result<ExactSqlBatchResult, ExactSqlError> {
        if sql.trim().is_empty() {
            return Err(ExactSqlError::InvalidStatement);
        }
        match self.dispatch_with_policy(
            SqlRequest::ExecuteBatch(sql),
            ExecutionPolicy::AuthorityRevalidated,
        )? {
            SqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(ExactSqlError::TransactionClosed),
        }
    }

    pub fn commit(mut self) -> Result<ExactSqlCommitReceipt, ExactSqlError> {
        let sender = self
            .commands
            .take()
            .ok_or(ExactSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Commit { reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    pub fn rollback(mut self) -> Result<ExactSqlRollbackReceipt, ExactSqlError> {
        let sender = self
            .commands
            .take()
            .ok_or(ExactSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Rollback { reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    fn dispatch(&self, request: SqlRequest) -> Result<SqlResult, ExactSqlError> {
        self.dispatch_with_policy(request, ExecutionPolicy::Bounded)
    }

    fn dispatch_with_policy(
        &self,
        request: SqlRequest,
        execution_policy: ExecutionPolicy,
    ) -> Result<SqlResult, ExactSqlError> {
        validate_request(&request)?;
        if execution_policy == ExecutionPolicy::AuthorityRevalidated
            && self.policy != TransactionPolicy::AuthorizedLongLease
        {
            return Err(ExactSqlError::AuthorityDenied(
                "authority-revalidated batches require an authority-bound long-lease transaction"
                    .to_owned(),
            ));
        }
        let sender = self
            .commands
            .as_ref()
            .ok_or(ExactSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Dispatch {
                request,
                execution_policy,
                reply,
            })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }
}

fn execute_request(
    connection: &Connection,
    request: SqlRequest,
    pinned_transaction: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
    enforce_statement_limit: bool,
    repeated_authority: Option<(Arc<dyn ExactSqlWriteAuthority>, ExactSqlWriteIntent)>,
) -> (Result<SqlResult, ExactSqlError>, bool) {
    if let Err(error) = validate_request(&request) {
        return (Err(error), false);
    }
    let insert_tracker = Arc::new(InsertTracker::default());
    let result = with_exact_sql_guard(
        connection,
        pinned_transaction,
        false,
        shutdown_requested,
        execution_deadline,
        enforce_statement_limit,
        repeated_authority,
        crate::connection::authorize_writer,
        true,
        None,
        Some(Arc::clone(&insert_tracker)),
        || match request {
            SqlRequest::Validate(statement) => connection
                .prepare(&statement.sql)
                .map(|_| SqlResult::Validated)
                .map_err(|error| sqlite_error("validate statement", error)),
            SqlRequest::Execute(statement) => {
                execute_statement(connection, statement).map(SqlResult::Executed)
            }
            SqlRequest::Query(statement) => {
                execute_query_unchecked(connection, statement).map(SqlResult::Queried)
            }
            SqlRequest::ExecuteBatch(sql) => {
                execute_batch(connection, &sql).map(SqlResult::BatchExecuted)
            }
        },
    );
    (result, insert_tracker.applied.load(Ordering::Acquire))
}

fn verify_write_authority(
    authority: Option<&dyn ExactSqlWriteAuthority>,
    intent: ExactSqlWriteIntent,
) -> Result<(), ExactSqlError> {
    match authority {
        Some(authority) => authority.verify(intent),
        None => Ok(()),
    }
}

fn publish_last_insert_rowid(
    result: &mut Result<SqlResult, ExactSqlError>,
    inserted: bool,
    connection_rowid: i64,
    logical_rowid: &AtomicI64,
) {
    if inserted {
        logical_rowid.store(connection_rowid, Ordering::Release);
    }
    let rowid = logical_rowid.load(Ordering::Acquire);
    match result.as_mut() {
        Ok(SqlResult::Executed(result)) => result.last_insert_rowid = rowid,
        Ok(SqlResult::BatchExecuted(result)) => result.last_insert_rowid = rowid,
        Ok(SqlResult::Validated | SqlResult::Queried(_)) | Err(_) => {}
    }
}

fn validate_request(request: &SqlRequest) -> Result<(), ExactSqlError> {
    match request {
        SqlRequest::Validate(statement)
        | SqlRequest::Execute(statement)
        | SqlRequest::Query(statement) => statement.validate(),
        SqlRequest::ExecuteBatch(sql) => validate_batch(sql),
    }
}

fn validate_batch(sql: &String) -> Result<(), ExactSqlError> {
    if sql.trim().is_empty() {
        Err(ExactSqlError::InvalidStatement)
    } else if sql.capacity() > MAX_SQL_BYTES {
        Err(ExactSqlError::RequestLimitExceeded)
    } else {
        Ok(())
    }
}

fn execute_statement(
    connection: &Connection,
    statement: ExactSqlStatement,
) -> Result<ExactSqlExecuteResult, ExactSqlError> {
    let values = statement
        .params
        .into_iter()
        .map(ExactSqlValue::into_rusqlite);
    let mut prepared = connection
        .prepare(&statement.sql)
        .map_err(|error| sqlite_error("prepare execute", error))?;
    let changed_rows = prepared
        .execute(params_from_iter(values))
        .map_err(|error| sqlite_error("execute", error))?;
    Ok(ExactSqlExecuteResult {
        changed_rows,
        last_insert_rowid: connection.last_insert_rowid(),
    })
}

fn attach_database(
    connection: &Connection,
    attachment: &ExactSqlAttachment,
    pinned_transaction: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
) -> Result<(), ExactSqlError> {
    let sql = format!("ATTACH DATABASE ?1 AS \"{}\"", attachment.database_name());
    let statement = ExactSqlStatement::new(
        sql,
        vec![ExactSqlValue::Text(attachment.filename().to_owned())],
    )?;
    with_exact_sql_guard(
        connection,
        pinned_transaction,
        false,
        shutdown_requested,
        execution_deadline,
        true,
        None,
        crate::connection::authorize_writer,
        true,
        Some(AuthorizedDatabaseOperation::Attach),
        None,
        || execute_statement(connection, statement).map(|_| ()),
    )
}

fn detach_database(
    connection: &Connection,
    database_name: &str,
    shutdown_requested: Option<Arc<AtomicBool>>,
) -> Result<(), ExactSqlError> {
    if !valid_database_name(database_name) {
        return Err(ExactSqlError::InvalidAttachment);
    }
    let sql = format!("DETACH DATABASE \"{database_name}\"");
    with_exact_sql_guard(
        connection,
        false,
        false,
        shutdown_requested,
        None,
        true,
        None,
        crate::connection::authorize_writer,
        true,
        Some(AuthorizedDatabaseOperation::Detach(
            database_name.to_owned(),
        )),
        None,
        || execute_batch(connection, &sql).map(|_| ()),
    )
}

fn execute_batch(connection: &Connection, sql: &str) -> Result<ExactSqlBatchResult, ExactSqlError> {
    let before = connection.total_changes();
    connection
        .execute_batch(sql)
        .map_err(|error| sqlite_error("execute batch", error))?;
    Ok(ExactSqlBatchResult {
        changed_rows: connection.total_changes().saturating_sub(before),
        last_insert_rowid: connection.last_insert_rowid(),
    })
}

pub(crate) fn execute_query(
    connection: &Connection,
    request: ExactSqlStatement,
) -> Result<ExactSqlRows, ExactSqlError> {
    request.validate()?;
    with_exact_sql_guard(
        connection,
        false,
        false,
        None,
        None,
        true,
        None,
        crate::connection::authorize_reader,
        false,
        None,
        None,
        || execute_query_unchecked(connection, request),
    )
}

fn execute_query_unchecked(
    connection: &Connection,
    request: ExactSqlStatement,
) -> Result<ExactSqlRows, ExactSqlError> {
    let mut statement = connection
        .prepare(&request.sql)
        .map_err(|error| sqlite_error("prepare query", error))?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let column_count = columns.len();
    let values = request.params.into_iter().map(ExactSqlValue::into_rusqlite);
    let mut query = statement
        .query(params_from_iter(values))
        .map_err(|error| sqlite_error("start query", error))?;
    let mut rows = Vec::new();
    let mut materialized_bytes = columns
        .iter()
        .try_fold(std::mem::size_of::<Vec<String>>(), |total, column| {
            total
                .checked_add(std::mem::size_of::<String>())
                .and_then(|total| total.checked_add(column.len()))
        })
        .ok_or(ExactSqlError::QueryLimitExceeded)?;
    while let Some(row) = query
        .next()
        .map_err(|error| sqlite_error("advance query", error))?
    {
        if rows.len() >= MAX_QUERY_ROWS {
            return Err(ExactSqlError::QueryLimitExceeded);
        }
        materialized_bytes = materialized_bytes
            .checked_add(ROW_ALLOCATION_OVERHEAD)
            .and_then(|total| {
                CELL_ALLOCATION_OVERHEAD
                    .checked_mul(column_count)
                    .and_then(|cells| total.checked_add(cells))
            })
            .ok_or(ExactSqlError::QueryLimitExceeded)?;
        if materialized_bytes > MAX_QUERY_BYTES {
            return Err(ExactSqlError::QueryLimitExceeded);
        }
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = ExactSqlValue::from_rusqlite(
                row.get_ref(index)
                    .map_err(|error| sqlite_error("read query value", error))?,
            )?;
            materialized_bytes = materialized_bytes
                .checked_add(value.materialized_bytes())
                .ok_or(ExactSqlError::QueryLimitExceeded)?;
            if materialized_bytes > MAX_QUERY_BYTES {
                return Err(ExactSqlError::QueryLimitExceeded);
            }
            values.push(value);
        }
        rows.push(ExactSqlRow { values });
    }
    Ok(ExactSqlRows { columns, rows })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> ExactSqlError {
    let (code, extended_code) = match &error {
        rusqlite::Error::SqliteFailure(error, _) => {
            (Some(error.extended_code & 0xff), Some(error.extended_code))
        }
        _ => (None, None),
    };
    ExactSqlError::Sqlite {
        operation,
        code,
        extended_code,
        message: error.to_string(),
    }
}

fn map_writer_send_error(error: tokio_mpsc::error::TrySendError<WriterCommand>) -> ExactSqlError {
    match error {
        tokio_mpsc::error::TrySendError::Full(_) => ExactSqlError::Busy,
        tokio_mpsc::error::TrySendError::Closed(_) => ExactSqlError::WriterUnavailable,
    }
}

fn transaction_terminal_error(expired: &AtomicBool) -> ExactSqlError {
    if expired.load(Ordering::Acquire) {
        ExactSqlError::TransactionExpired
    } else {
        ExactSqlError::TransactionClosed
    }
}

fn map_transaction_send_error(
    error: mpsc::TrySendError<TransactionCommand>,
    expired: &AtomicBool,
) -> ExactSqlError {
    match error {
        mpsc::TrySendError::Full(_) => ExactSqlError::Busy,
        mpsc::TrySendError::Disconnected(_) => transaction_terminal_error(expired),
    }
}

#[cfg(test)]
mod tests;
