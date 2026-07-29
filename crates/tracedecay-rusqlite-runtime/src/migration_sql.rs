//! Temporary internal SQL transport for the one-shot consolidated migration.
//!
//! Authority comes only from an already-attached writer and reader pool. This
//! module exposes owned values, never a SQLite connection or filesystem path.

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use rusqlite::{
    Connection, Transaction, TransactionBehavior,
    hooks::{Action, AuthAction, Authorization},
    limits::Limit,
    params_from_iter,
    types::{Value, ValueRef},
};
use tokio::sync::mpsc as tokio_mpsc;
use tracedecay_store::{StoreRuntimeBindingV1, UnavailableReasonV1, VerifiedStoreLocatorV1};

use crate::{
    PersistentWriter,
    reader::{
        ReaderAcquireError, ReaderPool, ReaderQueryExecutor, StoreSizeTelemetrySample,
        TableSizeTelemetrySample,
    },
};

const MAX_QUERY_ROWS: usize = 10_000;
const MAX_QUERY_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 1024 * 1024;
const MAX_SQL_PARAMETERS: usize = 32_766;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_MIGRATION_ATTACHMENTS: i32 = 4;
const MIGRATION_SQL_PROGRESS_INTERVAL_OPS: i32 = 1_000;
#[cfg(not(test))]
const MIGRATION_SQL_EXECUTION_LIMIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const MIGRATION_SQL_EXECUTION_LIMIT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const MIGRATION_SQL_TRANSACTION_IDLE_LIMIT: Duration = Duration::from_secs(30);
#[cfg(test)]
const MIGRATION_SQL_TRANSACTION_IDLE_LIMIT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const MIGRATION_SQL_TRANSACTION_LIMIT: Duration = Duration::from_secs(120);
#[cfg(test)]
const MIGRATION_SQL_TRANSACTION_LIMIT: Duration = Duration::from_millis(500);
const ROW_ALLOCATION_OVERHEAD: usize =
    std::mem::size_of::<MigrationSqlRow>() + std::mem::size_of::<Vec<MigrationSqlValue>>();
const CELL_ALLOCATION_OVERHEAD: usize = std::mem::size_of::<MigrationSqlValue>();

#[derive(Default)]
struct InsertTracker {
    authorized_tables: Mutex<BTreeSet<String>>,
    applied: AtomicBool,
}

#[derive(Clone)]
enum AuthorizedDatabaseOperation {
    Attach,
    Detach(String),
    Vacuum,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MigrationSqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl MigrationSqlValue {
    fn into_rusqlite(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(value) => Value::Integer(value),
            Self::Real(value) => Value::Real(value),
            Self::Text(value) => Value::Text(value),
            Self::Blob(value) => Value::Blob(value),
        }
    }

    fn from_rusqlite(value: ValueRef<'_>) -> Result<Self, MigrationSqlError> {
        Ok(match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value) => Self::Integer(value),
            ValueRef::Real(value) => Self::Real(value),
            ValueRef::Text(value) => Self::Text(
                std::str::from_utf8(value)
                    .map_err(|error| MigrationSqlError::Sqlite {
                        operation: "decode query text",
                        code: None,
                        extended_code: None,
                        message: error.to_string(),
                    })?
                    .to_owned(),
            ),
            ValueRef::Blob(value) => Self::Blob(value.to_vec()),
        })
    }

    fn materialized_bytes(&self) -> usize {
        match self {
            Self::Null => 0,
            Self::Integer(_) | Self::Real(_) => 8,
            Self::Text(value) => value.len(),
            Self::Blob(value) => value.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationSqlStatement {
    pub sql: String,
    pub params: Vec<MigrationSqlValue>,
}

impl MigrationSqlStatement {
    pub fn new(sql: String, params: Vec<MigrationSqlValue>) -> Result<Self, MigrationSqlError> {
        let statement = Self { sql, params };
        statement.validate()?;
        Ok(statement)
    }

    fn validate(&self) -> Result<(), MigrationSqlError> {
        if self.sql.trim().is_empty() {
            return Err(MigrationSqlError::InvalidStatement);
        }
        if self.sql.capacity() > MAX_SQL_BYTES || self.params.capacity() > MAX_SQL_PARAMETERS {
            return Err(MigrationSqlError::RequestLimitExceeded);
        }
        let bytes = CELL_ALLOCATION_OVERHEAD
            .checked_mul(self.params.capacity())
            .and_then(|params| self.sql.capacity().checked_add(params))
            .and_then(|initial| {
                self.params.iter().try_fold(initial, |total, value| {
                    let retained = match value {
                        MigrationSqlValue::Text(value) => value.capacity(),
                        MigrationSqlValue::Blob(value) => value.capacity(),
                        _ => value.materialized_bytes(),
                    };
                    total.checked_add(retained)
                })
            });
        if bytes.is_none_or(|bytes| bytes > MAX_REQUEST_BYTES) {
            return Err(MigrationSqlError::RequestLimitExceeded);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationSqlAttachment {
    filename: String,
    database_name: String,
}

impl MigrationSqlAttachment {
    pub fn new(
        filename: impl Into<String>,
        database_name: impl Into<String>,
    ) -> Result<Self, MigrationSqlError> {
        let filename = filename.into();
        let database_name = database_name.into();
        if filename.is_empty()
            || filename.len() > MAX_SQL_BYTES
            || !valid_database_name(&database_name)
        {
            return Err(MigrationSqlError::InvalidAttachment);
        }
        Ok(Self {
            filename,
            database_name,
        })
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn database_name(&self) -> &str {
        &self.database_name
    }
}

fn valid_database_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && !value.eq_ignore_ascii_case("main")
        && !value.eq_ignore_ascii_case("temp")
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationSqlRow {
    pub values: Vec<MigrationSqlValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationSqlRows {
    pub columns: Vec<String>,
    pub rows: Vec<MigrationSqlRow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSqlExecuteResult {
    pub changed_rows: usize,
    pub last_insert_rowid: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSqlBatchResult {
    pub changed_rows: u64,
    pub last_insert_rowid: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSqlCommitReceipt {
    pub changed_rows: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSqlRollbackReceipt {
    pub discarded_changed_rows: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MigrationSqlRequest {
    Validate(MigrationSqlStatement),
    Execute(MigrationSqlStatement),
    Query(MigrationSqlStatement),
    ExecuteBatch(String),
}

impl MigrationSqlRequest {
    fn intent(&self) -> MigrationSqlWriteIntent {
        match self {
            Self::Validate(_) => MigrationSqlWriteIntent::Validate,
            Self::Execute(_) => MigrationSqlWriteIntent::Execute,
            Self::Query(_) => MigrationSqlWriteIntent::Query,
            Self::ExecuteBatch(_) => MigrationSqlWriteIntent::ExecuteBatch,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MigrationSqlResult {
    Validated,
    Executed(MigrationSqlExecuteResult),
    Queried(MigrationSqlRows),
    BatchExecuted(MigrationSqlBatchResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationSqlWriteIntent {
    Validate,
    Execute,
    Query,
    ExecuteBatch,
    Vacuum,
    BeginTransaction,
    Commit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MigrationSqlTransactionPolicy {
    Ordinary,
    SchemaMigration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MigrationSqlStepPolicy {
    Bounded,
    AuthorizedLongSchema,
}

pub trait MigrationSqlWriteAuthority: Send + Sync {
    fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationSqlError {
    AuthorityMismatch,
    AuthorityDenied(String),
    InvalidAttachment,
    InvalidStatement,
    RequestLimitExceeded,
    TransactionControlDenied,
    QueryLimitExceeded,
    Busy,
    WriterUnavailable,
    ReaderUnavailable(String),
    TransactionClosed,
    TransactionExpired,
    Sqlite {
        operation: &'static str,
        code: Option<i32>,
        extended_code: Option<i32>,
        message: String,
    },
}

impl fmt::Display for MigrationSqlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityMismatch => {
                formatter.write_str("migration SQL authority does not match attached runtime")
            }
            Self::AuthorityDenied(reason) => {
                write!(formatter, "migration SQL write authority denied: {reason}")
            }
            Self::InvalidAttachment => formatter.write_str("migration SQL attachment is invalid"),
            Self::InvalidStatement => formatter.write_str("migration SQL statement is empty"),
            Self::RequestLimitExceeded => {
                formatter.write_str("migration SQL request exceeded its admission limit")
            }
            Self::TransactionControlDenied => {
                formatter.write_str("transaction control SQL is denied on the migration channel")
            }
            Self::QueryLimitExceeded => {
                formatter.write_str("migration SQL query materialization exceeded its limit")
            }
            Self::Busy => formatter.write_str("migration SQL channel is busy"),
            Self::WriterUnavailable => formatter.write_str("migration SQL writer is unavailable"),
            Self::ReaderUnavailable(message) => {
                write!(formatter, "migration SQL reader is unavailable: {message}")
            }
            Self::TransactionClosed => {
                formatter.write_str("migration SQL transaction is already closed")
            }
            Self::TransactionExpired => {
                formatter.write_str("migration SQL transaction lease expired")
            }
            Self::Sqlite {
                operation, message, ..
            } => {
                write!(formatter, "migration SQL {operation} failed: {message}")
            }
        }
    }
}

impl Error for MigrationSqlError {}

type MigrationQuery = dyn Fn(MigrationSqlStatement, Duration) -> Result<MigrationSqlRows, MigrationSqlError>
    + Send
    + Sync;
type MigrationSnapshotFactory =
    dyn Fn(Duration) -> Result<MigrationSqlReadSnapshot, MigrationSqlError> + Send + Sync;
type MigrationSnapshotQuery =
    dyn FnMut(MigrationSqlStatement) -> Result<MigrationSqlRows, MigrationSqlError> + Send;
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
pub struct MigrationSqlHandle {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    writer: Option<tokio_mpsc::Sender<WriterCommand>>,
    query: Arc<MigrationQuery>,
    snapshot: Arc<MigrationSnapshotFactory>,
    health_snapshot: Arc<MigrationSnapshotFactory>,
    store_size_telemetry: Arc<StoreSizeTelemetryRead>,
    table_size_telemetry: Arc<TableSizeTelemetryRead>,
    last_insert_rowid: Arc<AtomicI64>,
    write_authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
}

impl MigrationSqlHandle {
    pub fn attach<E: ReaderQueryExecutor>(
        writer: &PersistentWriter,
        readers: &ReaderPool<E>,
    ) -> Result<Self, MigrationSqlError> {
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
            return Err(MigrationSqlError::AuthorityMismatch);
        }
        let sender = writer
            .migration_sql_sender()
            .ok_or(MigrationSqlError::WriterUnavailable)?;
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
        Self {
            binding,
            locator,
            writer,
            query: Arc::new(move |statement, max_wait| {
                query_readers
                    .upgrade()
                    .ok_or_else(|| {
                        MigrationSqlError::ReaderUnavailable(
                            "migration SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .execute_migration_query(statement, max_wait)
            }),
            snapshot: Arc::new(move |max_wait| {
                snapshot_readers
                    .upgrade()
                    .ok_or_else(|| {
                        MigrationSqlError::ReaderUnavailable(
                            "migration SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .begin_migration_snapshot(max_wait)
            }),
            health_snapshot: Arc::new(move |max_wait| {
                health_snapshot_readers
                    .upgrade()
                    .ok_or_else(|| {
                        MigrationSqlError::ReaderUnavailable(
                            "migration SQL reader pool is closed".to_owned(),
                        )
                    })?
                    .begin_migration_health_snapshot(max_wait)
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
            last_insert_rowid: Arc::clone(&self.last_insert_rowid),
            write_authority: None,
        }
    }

    pub fn with_write_authority(
        mut self,
        authority: Arc<dyn MigrationSqlWriteAuthority>,
    ) -> Result<Self, MigrationSqlError> {
        if self.writer.is_none() {
            return Err(MigrationSqlError::WriterUnavailable);
        }
        self.write_authority = Some(authority);
        Ok(self)
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid.load(Ordering::Acquire)
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
        request: MigrationSqlRequest,
        reader_wait: Duration,
    ) -> Result<MigrationSqlResult, MigrationSqlError> {
        match request {
            MigrationSqlRequest::Query(statement) => self
                .query(statement, reader_wait)
                .map(MigrationSqlResult::Queried),
            request => self.dispatch_writer(request),
        }
    }

    pub fn execute(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlExecuteResult, MigrationSqlError> {
        match self.dispatch_writer(MigrationSqlRequest::Execute(statement))? {
            MigrationSqlResult::Executed(result) => Ok(result),
            _ => Err(MigrationSqlError::WriterUnavailable),
        }
    }

    pub fn validate(&self, statement: MigrationSqlStatement) -> Result<(), MigrationSqlError> {
        match self.dispatch_writer(MigrationSqlRequest::Validate(statement))? {
            MigrationSqlResult::Validated => Ok(()),
            _ => Err(MigrationSqlError::WriterUnavailable),
        }
    }

    pub fn query(
        &self,
        statement: MigrationSqlStatement,
        max_wait: Duration,
    ) -> Result<MigrationSqlRows, MigrationSqlError> {
        statement.validate()?;
        (self.query)(statement, max_wait)
    }

    /// Checkpoints and truncates the WAL on the serialized writer connection.
    pub fn checkpoint_wal_truncate(&self) -> Result<MigrationSqlRows, MigrationSqlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(MigrationSqlError::WriterUnavailable)?
            .try_send(WriterCommand::CheckpointWalTruncate {
                reply,
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| MigrationSqlError::WriterUnavailable)?
    }

    pub fn execute_batch(&self, sql: String) -> Result<MigrationSqlBatchResult, MigrationSqlError> {
        validate_batch(&sql)?;
        match self.dispatch_writer(MigrationSqlRequest::ExecuteBatch(sql))? {
            MigrationSqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(MigrationSqlError::WriterUnavailable),
        }
    }

    /// Enables incremental auto-vacuum through its fixed maintenance rebuild.
    pub fn repair_incremental_auto_vacuum(&self) -> Result<(), MigrationSqlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(MigrationSqlError::WriterUnavailable)?
            .try_send(WriterCommand::Vacuum {
                reply,
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| MigrationSqlError::WriterUnavailable)?
    }

    pub fn begin_read_snapshot(
        &self,
        max_wait: Duration,
    ) -> Result<MigrationSqlReadSnapshot, MigrationSqlError> {
        (self.snapshot)(max_wait)
    }

    pub fn begin_health_read_snapshot(
        &self,
        max_wait: Duration,
    ) -> Result<MigrationSqlReadSnapshot, MigrationSqlError> {
        (self.health_snapshot)(max_wait)
    }

    pub fn begin_immediate(&self) -> Result<MigrationSqlTransaction, MigrationSqlError> {
        self.begin_transaction(
            TransactionBehavior::Immediate,
            MigrationSqlTransactionPolicy::Ordinary,
        )
    }

    pub fn begin_deferred(&self) -> Result<MigrationSqlTransaction, MigrationSqlError> {
        self.begin_transaction(
            TransactionBehavior::Deferred,
            MigrationSqlTransactionPolicy::Ordinary,
        )
    }

    /// Begins the only transaction mode permitted to run an explicitly
    /// authorized schema step without the ordinary statement deadline.
    ///
    /// The mode is intentionally not configurable: callers must attach a live
    /// write authority and opt into the schema-specific transaction and step
    /// APIs. Shutdown and authority revocation remain progress-handler
    /// cancellation conditions.
    pub fn begin_schema_migration_immediate(
        &self,
    ) -> Result<MigrationSqlTransaction, MigrationSqlError> {
        if self.writer.is_none() {
            return Err(MigrationSqlError::WriterUnavailable);
        }
        if self.write_authority.is_none() {
            return Err(MigrationSqlError::AuthorityDenied(
                "schema migration transaction requires attached write authority".to_owned(),
            ));
        }
        self.begin_transaction(
            TransactionBehavior::Immediate,
            MigrationSqlTransactionPolicy::SchemaMigration,
        )
    }

    fn begin_transaction(
        &self,
        behavior: TransactionBehavior,
        policy: MigrationSqlTransactionPolicy,
    ) -> Result<MigrationSqlTransaction, MigrationSqlError> {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (reply, response) = mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        self.writer
            .as_ref()
            .ok_or(MigrationSqlError::WriterUnavailable)?
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
            .map_err(|_| MigrationSqlError::WriterUnavailable)??;
        Ok(MigrationSqlTransaction {
            commands: Some(commands),
            expired,
            policy,
        })
    }

    fn dispatch_writer(
        &self,
        request: MigrationSqlRequest,
    ) -> Result<MigrationSqlResult, MigrationSqlError> {
        validate_request(&request)?;
        let (reply, response) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or(MigrationSqlError::WriterUnavailable)?
            .try_send(WriterCommand::Dispatch {
                request,
                reply,
                last_insert_rowid: Arc::clone(&self.last_insert_rowid),
                authority: self.write_authority.clone(),
            })
            .map_err(map_writer_send_error)?;
        response
            .recv()
            .map_err(|_| MigrationSqlError::WriterUnavailable)?
    }
}

pub struct MigrationSqlReadSnapshot {
    query: std::sync::Mutex<Box<MigrationSnapshotQuery>>,
}

impl MigrationSqlReadSnapshot {
    pub(crate) fn new<F>(query: F) -> Self
    where
        F: FnMut(MigrationSqlStatement) -> Result<MigrationSqlRows, MigrationSqlError>
            + Send
            + 'static,
    {
        Self {
            query: std::sync::Mutex::new(Box::new(query)),
        }
    }

    pub fn query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, MigrationSqlError> {
        statement.validate()?;
        self.query
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)(statement)
    }
}

pub struct MigrationSqlTransaction {
    commands: Option<mpsc::SyncSender<TransactionCommand>>,
    expired: Arc<AtomicBool>,
    policy: MigrationSqlTransactionPolicy,
}

impl MigrationSqlTransaction {
    pub fn attach_database(
        &self,
        attachment: MigrationSqlAttachment,
    ) -> Result<(), MigrationSqlError> {
        let sender = self
            .commands
            .as_ref()
            .ok_or(MigrationSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Attach { attachment, reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    pub fn validate(&self, statement: MigrationSqlStatement) -> Result<(), MigrationSqlError> {
        match self.dispatch(MigrationSqlRequest::Validate(statement))? {
            MigrationSqlResult::Validated => Ok(()),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    pub fn execute(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlExecuteResult, MigrationSqlError> {
        match self.dispatch(MigrationSqlRequest::Execute(statement))? {
            MigrationSqlResult::Executed(result) => Ok(result),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    pub fn query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, MigrationSqlError> {
        match self.dispatch(MigrationSqlRequest::Query(statement))? {
            MigrationSqlResult::Queried(result) => Ok(result),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    pub fn execute_batch(&self, sql: String) -> Result<MigrationSqlBatchResult, MigrationSqlError> {
        if sql.trim().is_empty() {
            return Err(MigrationSqlError::InvalidStatement);
        }
        match self.dispatch(MigrationSqlRequest::ExecuteBatch(sql))? {
            MigrationSqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    /// Executes one schema statement without the ordinary statement deadline.
    ///
    /// This is accepted only on an authority-bound schema migration
    /// transaction. The writer actor re-verifies that authority before,
    /// repeatedly during, and after execution.
    pub fn execute_schema_step(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlExecuteResult, MigrationSqlError> {
        match self.dispatch_with_policy(
            MigrationSqlRequest::Execute(statement),
            MigrationSqlStepPolicy::AuthorizedLongSchema,
        )? {
            MigrationSqlResult::Executed(result) => Ok(result),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    /// Executes one schema batch without the ordinary statement deadline.
    ///
    /// This is not a generic unbounded mode; it is accepted only by an
    /// authority-bound schema migration transaction.
    pub fn execute_schema_batch_step(
        &self,
        sql: String,
    ) -> Result<MigrationSqlBatchResult, MigrationSqlError> {
        if sql.trim().is_empty() {
            return Err(MigrationSqlError::InvalidStatement);
        }
        match self.dispatch_with_policy(
            MigrationSqlRequest::ExecuteBatch(sql),
            MigrationSqlStepPolicy::AuthorizedLongSchema,
        )? {
            MigrationSqlResult::BatchExecuted(result) => Ok(result),
            _ => Err(MigrationSqlError::TransactionClosed),
        }
    }

    pub fn commit(mut self) -> Result<MigrationSqlCommitReceipt, MigrationSqlError> {
        let sender = self
            .commands
            .take()
            .ok_or(MigrationSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Commit { reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    pub fn rollback(mut self) -> Result<MigrationSqlRollbackReceipt, MigrationSqlError> {
        let sender = self
            .commands
            .take()
            .ok_or(MigrationSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Rollback { reply })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }

    fn dispatch(
        &self,
        request: MigrationSqlRequest,
    ) -> Result<MigrationSqlResult, MigrationSqlError> {
        self.dispatch_with_policy(request, MigrationSqlStepPolicy::Bounded)
    }

    fn dispatch_with_policy(
        &self,
        request: MigrationSqlRequest,
        step_policy: MigrationSqlStepPolicy,
    ) -> Result<MigrationSqlResult, MigrationSqlError> {
        validate_request(&request)?;
        if step_policy == MigrationSqlStepPolicy::AuthorizedLongSchema
            && self.policy != MigrationSqlTransactionPolicy::SchemaMigration
        {
            return Err(MigrationSqlError::AuthorityDenied(
                "long schema steps require an authority-bound schema migration transaction"
                    .to_owned(),
            ));
        }
        let sender = self
            .commands
            .as_ref()
            .ok_or(MigrationSqlError::TransactionClosed)?;
        let (reply, response) = mpsc::sync_channel(1);
        sender
            .try_send(TransactionCommand::Dispatch {
                request,
                step_policy,
                reply,
            })
            .map_err(|error| map_transaction_send_error(error, &self.expired))?;
        response
            .recv()
            .map_err(|_| transaction_terminal_error(&self.expired))?
    }
}

pub(crate) enum WriterCommand {
    Dispatch {
        request: MigrationSqlRequest,
        reply: SyncSender<Result<MigrationSqlResult, MigrationSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    BeginTransaction {
        behavior: TransactionBehavior,
        policy: MigrationSqlTransactionPolicy,
        receiver: Receiver<TransactionCommand>,
        reply: SyncSender<Result<(), MigrationSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        expired: Arc<AtomicBool>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    CheckpointWalTruncate {
        reply: SyncSender<Result<MigrationSqlRows, MigrationSqlError>>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    Vacuum {
        reply: SyncSender<Result<(), MigrationSqlError>>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
}

pub(crate) enum TransactionCommand {
    Attach {
        attachment: MigrationSqlAttachment,
        reply: SyncSender<Result<(), MigrationSqlError>>,
    },
    Dispatch {
        request: MigrationSqlRequest,
        step_policy: MigrationSqlStepPolicy,
        reply: SyncSender<Result<MigrationSqlResult, MigrationSqlError>>,
    },
    Commit {
        reply: SyncSender<Result<MigrationSqlCommitReceipt, MigrationSqlError>>,
    },
    Rollback {
        reply: SyncSender<Result<MigrationSqlRollbackReceipt, MigrationSqlError>>,
    },
}

pub(crate) fn run_writer_command(
    connection: &mut Connection,
    command: WriterCommand,
    shutdown_requested: &Arc<AtomicBool>,
) {
    match command {
        WriterCommand::Dispatch {
            request,
            reply,
            last_insert_rowid,
            authority,
        } => {
            if let Err(error) = verify_write_authority(authority.as_deref(), request.intent()) {
                let _ = reply.send(Err(error));
                return;
            }
            let (mut result, inserted) = execute_request(
                connection,
                request,
                false,
                Some(Arc::clone(shutdown_requested)),
                None,
                true,
                None,
            );
            publish_last_insert_rowid(
                &mut result,
                inserted,
                connection.last_insert_rowid(),
                &last_insert_rowid,
            );
            let _ = reply.send(result);
        }
        WriterCommand::BeginTransaction {
            behavior,
            policy,
            receiver,
            reply,
            last_insert_rowid,
            expired,
            authority,
        } => {
            if policy == MigrationSqlTransactionPolicy::SchemaMigration && authority.is_none() {
                let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                    "schema migration transaction requires attached write authority".to_owned(),
                )));
                return;
            }
            if let Err(error) = verify_write_authority(
                authority.as_deref(),
                MigrationSqlWriteIntent::BeginTransaction,
            ) {
                let _ = reply.send(Err(error));
                return;
            }
            let completion = {
                let before = connection.total_changes();
                match connection.transaction_with_behavior(behavior) {
                    Ok(transaction) if reply.send(Ok(())).is_ok() => Some(run_transaction(
                        transaction,
                        receiver,
                        before,
                        shutdown_requested,
                        &last_insert_rowid,
                        &expired,
                        authority,
                        policy,
                    )),
                    Ok(_) => None,
                    Err(error) => {
                        let _ = reply.send(Err(sqlite_error("begin migration transaction", error)));
                        None
                    }
                }
            };
            if completion.is_some_and(|completion| completion.finish(connection).is_err()) {
                shutdown_requested.store(true, Ordering::Release);
            }
        }
        WriterCommand::CheckpointWalTruncate { reply, authority } => {
            if let Err(error) =
                verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Query)
            {
                let _ = reply.send(Err(error));
                return;
            }
            let statement = MigrationSqlStatement::new(
                "PRAGMA wal_checkpoint(TRUNCATE)".to_owned(),
                Vec::new(),
            )
            .expect("fixed WAL checkpoint statement is valid");
            let result = with_migration_guard(
                connection,
                false,
                false,
                Some(Arc::clone(shutdown_requested)),
                None,
                true,
                None,
                crate::connection::authorize_writer,
                false,
                None,
                None,
                || execute_query_unchecked(connection, statement),
            );
            let _ = reply.send(result);
        }
        WriterCommand::Vacuum { reply, authority } => {
            let Some(authority) = authority else {
                let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                    "exclusive-maintenance vacuum requires attached write authority".to_owned(),
                )));
                return;
            };
            if let Err(error) =
                verify_write_authority(Some(authority.as_ref()), MigrationSqlWriteIntent::Vacuum)
            {
                let _ = reply.send(Err(error));
                return;
            }
            let previous_attachment_limit =
                match connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 1) {
                    Ok(previous) => previous,
                    Err(error) => {
                        let _ = reply.send(Err(sqlite_error(
                            "open exclusive-maintenance vacuum attachment slot",
                            error,
                        )));
                        return;
                    }
                };
            let mut result = with_migration_guard(
                connection,
                false,
                true,
                Some(Arc::clone(shutdown_requested)),
                None,
                true,
                Some((Arc::clone(&authority), MigrationSqlWriteIntent::Vacuum)),
                crate::connection::authorize_writer,
                true,
                Some(AuthorizedDatabaseOperation::Vacuum),
                None,
                || {
                    execute_batch(connection, "PRAGMA auto_vacuum = INCREMENTAL; VACUUM")
                        .map(|_| ())
                },
            );
            if let Err(error) =
                connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, previous_attachment_limit)
            {
                shutdown_requested.store(true, Ordering::Release);
                if result.is_ok() {
                    result = Err(sqlite_error(
                        "restore exclusive-maintenance vacuum attachment limit",
                        error,
                    ));
                }
            }
            let _ = reply.send(result);
        }
    }
}

pub(crate) fn reject_writer_command(command: WriterCommand) {
    match command {
        WriterCommand::Dispatch { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::BeginTransaction { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::CheckpointWalTruncate { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::Vacuum { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_transaction(
    transaction: Transaction<'_>,
    receiver: Receiver<TransactionCommand>,
    before: u64,
    shutdown_requested: &Arc<AtomicBool>,
    last_insert_rowid: &AtomicI64,
    expired: &AtomicBool,
    authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    policy: MigrationSqlTransactionPolicy,
) -> TransactionCompletion {
    let mut attachments = Vec::new();
    let mut previous_attachment_limit = None;
    let mut idle_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
    let mut transaction_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_LIMIT;
    loop {
        if shutdown_requested.load(Ordering::Acquire) {
            let _ = transaction.rollback();
            return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
        }
        let now = Instant::now();
        if now >= idle_deadline || now >= transaction_deadline {
            expired.store(true, Ordering::Release);
            let _ = transaction.rollback();
            return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
        }
        let wait = idle_deadline
            .saturating_duration_since(now)
            .min(transaction_deadline.saturating_duration_since(now))
            .min(Duration::from_millis(25));
        let command = match receiver.recv_timeout(wait) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
            }
        };
        match command {
            TransactionCommand::Attach { attachment, reply } => {
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if attachments.iter().any(|attached: &MigrationSqlAttachment| {
                    attached
                        .database_name()
                        .eq_ignore_ascii_case(attachment.database_name())
                }) {
                    let _ = reply.send(Err(MigrationSqlError::InvalidAttachment));
                    continue;
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Execute)
                {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if previous_attachment_limit.is_none() {
                    match transaction
                        .set_limit(Limit::SQLITE_LIMIT_ATTACHED, MAX_MIGRATION_ATTACHMENTS)
                    {
                        Ok(previous) => previous_attachment_limit = Some(previous),
                        Err(error) => {
                            let _ = transaction.rollback();
                            let _ = reply
                                .send(Err(sqlite_error("open migration attachment limit", error)));
                            return TransactionCompletion::abandoned(attachments, None);
                        }
                    }
                }
                let result = attach_database(
                    &transaction,
                    &attachment,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    Some(transaction_deadline),
                );
                match result {
                    Ok(()) => {
                        attachments.push(attachment);
                        if let Err(error) = verify_write_authority(
                            authority.as_deref(),
                            MigrationSqlWriteIntent::Execute,
                        ) {
                            let _ = transaction.rollback();
                            let _ = reply.send(Err(error));
                            return TransactionCompletion::abandoned(
                                attachments,
                                previous_attachment_limit,
                            );
                        }
                        let _ = reply.send(Ok(()));
                        idle_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
                    }
                    Err(error) => {
                        let _ = transaction.rollback();
                        let _ = reply.send(Err(error));
                        return TransactionCompletion::abandoned(
                            attachments,
                            previous_attachment_limit,
                        );
                    }
                }
            }
            TransactionCommand::Dispatch {
                request,
                step_policy,
                reply,
            } => {
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), request.intent()) {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if step_policy == MigrationSqlStepPolicy::AuthorizedLongSchema
                    && policy != MigrationSqlTransactionPolicy::SchemaMigration
                {
                    let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                        "long schema steps require an authority-bound schema migration transaction"
                            .to_owned(),
                    )));
                    continue;
                }
                let intent = request.intent();
                let repeated_authority =
                    (step_policy == MigrationSqlStepPolicy::AuthorizedLongSchema).then(|| {
                        (
                            Arc::clone(authority.as_ref().expect("schema authority")),
                            intent,
                        )
                    });
                let execution_deadline = (step_policy == MigrationSqlStepPolicy::Bounded)
                    .then_some(transaction_deadline);
                let (mut result, inserted) = execute_request(
                    &transaction,
                    request,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    execution_deadline,
                    step_policy == MigrationSqlStepPolicy::Bounded,
                    repeated_authority,
                );
                if shutdown_requested.load(Ordering::Acquire) {
                    let _ = transaction.rollback();
                    let _ = reply.send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), intent) {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if matches!(&result, Err(MigrationSqlError::AuthorityDenied(_))) {
                    let _ = transaction.rollback();
                    let _ = reply.send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if step_policy == MigrationSqlStepPolicy::Bounded
                    && Instant::now() >= transaction_deadline
                {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                publish_last_insert_rowid(
                    &mut result,
                    inserted,
                    transaction.last_insert_rowid(),
                    last_insert_rowid,
                );
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                if succeeded {
                    let renewed_at = Instant::now();
                    idle_deadline = renewed_at + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
                    if policy == MigrationSqlTransactionPolicy::SchemaMigration {
                        transaction_deadline = renewed_at + MIGRATION_SQL_TRANSACTION_LIMIT;
                    }
                }
            }
            TransactionCommand::Commit { reply } => {
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Commit)
                {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                let changed_rows = transaction.total_changes().saturating_sub(before);
                let result = transaction
                    .commit()
                    .map(|()| MigrationSqlCommitReceipt { changed_rows })
                    .map_err(|error| sqlite_error("commit immediate transaction", error));
                return TransactionCompletion {
                    attachments,
                    previous_attachment_limit,
                    terminal: Some(TransactionTerminal::Commit { reply, result }),
                };
            }
            TransactionCommand::Rollback { reply } => {
                let discarded_changed_rows = transaction.total_changes().saturating_sub(before);
                let result = transaction
                    .rollback()
                    .map(|()| MigrationSqlRollbackReceipt {
                        discarded_changed_rows,
                    })
                    .map_err(|error| sqlite_error("rollback immediate transaction", error));
                return TransactionCompletion {
                    attachments,
                    previous_attachment_limit,
                    terminal: Some(TransactionTerminal::Rollback { reply, result }),
                };
            }
        }
    }
}

struct TransactionCompletion {
    attachments: Vec<MigrationSqlAttachment>,
    previous_attachment_limit: Option<i32>,
    terminal: Option<TransactionTerminal>,
}

enum TransactionTerminal {
    Commit {
        reply: SyncSender<Result<MigrationSqlCommitReceipt, MigrationSqlError>>,
        result: Result<MigrationSqlCommitReceipt, MigrationSqlError>,
    },
    Rollback {
        reply: SyncSender<Result<MigrationSqlRollbackReceipt, MigrationSqlError>>,
        result: Result<MigrationSqlRollbackReceipt, MigrationSqlError>,
    },
}

impl TransactionCompletion {
    fn abandoned(
        attachments: Vec<MigrationSqlAttachment>,
        previous_attachment_limit: Option<i32>,
    ) -> Self {
        Self {
            attachments,
            previous_attachment_limit,
            terminal: None,
        }
    }

    fn finish(self, connection: &Connection) -> Result<(), MigrationSqlError> {
        let mut cleanup_error = None;
        for attachment in self.attachments.into_iter().rev() {
            if let Err(error) = detach_database(connection, attachment.database_name(), None)
                && cleanup_error.is_none()
            {
                cleanup_error = Some(error);
            }
        }
        if let Some(previous) = self.previous_attachment_limit
            && let Err(error) = connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, previous)
            && cleanup_error.is_none()
        {
            cleanup_error = Some(sqlite_error("restore migration attachment limit", error));
        }
        match self.terminal {
            Some(TransactionTerminal::Commit { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.send(response);
            }
            Some(TransactionTerminal::Rollback { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.send(response);
            }
            None => {}
        }
        cleanup_error.map_or(Ok(()), Err)
    }
}

fn execute_request(
    connection: &Connection,
    request: MigrationSqlRequest,
    pinned_transaction: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
    enforce_statement_limit: bool,
    repeated_authority: Option<(Arc<dyn MigrationSqlWriteAuthority>, MigrationSqlWriteIntent)>,
) -> (Result<MigrationSqlResult, MigrationSqlError>, bool) {
    if let Err(error) = validate_request(&request) {
        return (Err(error), false);
    }
    let insert_tracker = Arc::new(InsertTracker::default());
    let result = with_migration_guard(
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
            MigrationSqlRequest::Validate(statement) => connection
                .prepare(&statement.sql)
                .map(|_| MigrationSqlResult::Validated)
                .map_err(|error| sqlite_error("validate statement", error)),
            MigrationSqlRequest::Execute(statement) => {
                execute_statement(connection, statement).map(MigrationSqlResult::Executed)
            }
            MigrationSqlRequest::Query(statement) => {
                execute_query_unchecked(connection, statement).map(MigrationSqlResult::Queried)
            }
            MigrationSqlRequest::ExecuteBatch(sql) => {
                execute_batch(connection, &sql).map(MigrationSqlResult::BatchExecuted)
            }
        },
    );
    (result, insert_tracker.applied.load(Ordering::Acquire))
}

fn verify_write_authority(
    authority: Option<&dyn MigrationSqlWriteAuthority>,
    intent: MigrationSqlWriteIntent,
) -> Result<(), MigrationSqlError> {
    match authority {
        Some(authority) => authority.verify(intent),
        None => Ok(()),
    }
}

fn publish_last_insert_rowid(
    result: &mut Result<MigrationSqlResult, MigrationSqlError>,
    inserted: bool,
    connection_rowid: i64,
    logical_rowid: &AtomicI64,
) {
    if inserted {
        logical_rowid.store(connection_rowid, Ordering::Release);
    }
    let rowid = logical_rowid.load(Ordering::Acquire);
    match result.as_mut() {
        Ok(MigrationSqlResult::Executed(result)) => result.last_insert_rowid = rowid,
        Ok(MigrationSqlResult::BatchExecuted(result)) => result.last_insert_rowid = rowid,
        Ok(MigrationSqlResult::Validated | MigrationSqlResult::Queried(_)) | Err(_) => {}
    }
}

fn validate_request(request: &MigrationSqlRequest) -> Result<(), MigrationSqlError> {
    match request {
        MigrationSqlRequest::Validate(statement)
        | MigrationSqlRequest::Execute(statement)
        | MigrationSqlRequest::Query(statement) => statement.validate(),
        MigrationSqlRequest::ExecuteBatch(sql) => validate_batch(sql),
    }
}

fn validate_batch(sql: &String) -> Result<(), MigrationSqlError> {
    if sql.trim().is_empty() {
        Err(MigrationSqlError::InvalidStatement)
    } else if sql.capacity() > MAX_SQL_BYTES {
        Err(MigrationSqlError::RequestLimitExceeded)
    } else {
        Ok(())
    }
}

fn execute_statement(
    connection: &Connection,
    statement: MigrationSqlStatement,
) -> Result<MigrationSqlExecuteResult, MigrationSqlError> {
    let values = statement
        .params
        .into_iter()
        .map(MigrationSqlValue::into_rusqlite);
    let mut prepared = connection
        .prepare(&statement.sql)
        .map_err(|error| sqlite_error("prepare execute", error))?;
    let changed_rows = prepared
        .execute(params_from_iter(values))
        .map_err(|error| sqlite_error("execute", error))?;
    Ok(MigrationSqlExecuteResult {
        changed_rows,
        last_insert_rowid: connection.last_insert_rowid(),
    })
}

fn attach_database(
    connection: &Connection,
    attachment: &MigrationSqlAttachment,
    pinned_transaction: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
) -> Result<(), MigrationSqlError> {
    let sql = format!("ATTACH DATABASE ?1 AS \"{}\"", attachment.database_name());
    let statement = MigrationSqlStatement::new(
        sql,
        vec![MigrationSqlValue::Text(attachment.filename().to_owned())],
    )?;
    with_migration_guard(
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
) -> Result<(), MigrationSqlError> {
    if !valid_database_name(database_name) {
        return Err(MigrationSqlError::InvalidAttachment);
    }
    let sql = format!("DETACH DATABASE \"{database_name}\"");
    with_migration_guard(
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

fn execute_batch(
    connection: &Connection,
    sql: &str,
) -> Result<MigrationSqlBatchResult, MigrationSqlError> {
    let before = connection.total_changes();
    connection
        .execute_batch(sql)
        .map_err(|error| sqlite_error("execute batch", error))?;
    Ok(MigrationSqlBatchResult {
        changed_rows: connection.total_changes().saturating_sub(before),
        last_insert_rowid: connection.last_insert_rowid(),
    })
}

#[allow(clippy::too_many_arguments)]
fn with_migration_guard<T, F>(
    connection: &Connection,
    allow_savepoints: bool,
    allow_transactions: bool,
    shutdown_requested: Option<Arc<AtomicBool>>,
    execution_deadline: Option<Instant>,
    enforce_statement_limit: bool,
    repeated_authority: Option<(Arc<dyn MigrationSqlWriteAuthority>, MigrationSqlWriteIntent)>,
    canonical_authorizer: for<'a> fn(rusqlite::hooks::AuthContext<'a>) -> Authorization,
    migration_writer: bool,
    database_operation: Option<AuthorizedDatabaseOperation>,
    insert_tracker: Option<Arc<InsertTracker>>,
    operation: F,
) -> Result<T, MigrationSqlError>
where
    F: FnOnce() -> Result<T, MigrationSqlError>,
{
    let denied = Arc::new(AtomicBool::new(false));
    let hook_denied = Arc::clone(&denied);
    let authorizer_tracker = insert_tracker.clone();
    let authorized_database_operation = database_operation.clone();
    connection
        .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
            if context.accessor.is_none()
                && let AuthAction::Insert { table_name } = context.action
                && !table_name.eq_ignore_ascii_case("sqlite_master")
                && !table_name.eq_ignore_ascii_case("sqlite_schema")
                && let Some(tracker) = &authorizer_tracker
            {
                tracker
                    .authorized_tables
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(table_name.to_owned());
            }
            if (!allow_transactions && matches!(context.action, AuthAction::Transaction { .. }))
                || (!allow_savepoints && matches!(context.action, AuthAction::Savepoint { .. }))
            {
                hook_denied.store(true, Ordering::Release);
                Authorization::Deny
            } else if migration_writer {
                authorize_migration_writer(context, authorized_database_operation.as_ref())
            } else {
                canonical_authorizer(context)
            }
        }))
        .map_err(|error| sqlite_error("install transaction-control guard", error))?;
    if let Some(tracker) = &insert_tracker {
        let hook_tracker = Arc::clone(tracker);
        if let Err(error) = connection.update_hook(Some(
            move |action: Action, _database: &str, table: &str, _rowid: i64| {
                if action == Action::SQLITE_INSERT
                    && hook_tracker
                        .authorized_tables
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .contains(table)
                {
                    hook_tracker.applied.store(true, Ordering::Release);
                }
            },
        )) {
            let _ = connection.authorizer(Some(canonical_authorizer));
            return Err(sqlite_error("install insert tracker", error));
        }
    }
    let deadline = if enforce_statement_limit {
        let operation_deadline = Instant::now() + MIGRATION_SQL_EXECUTION_LIMIT;
        Some(
            execution_deadline
                .map(|deadline| deadline.min(operation_deadline))
                .unwrap_or(operation_deadline),
        )
    } else {
        execution_deadline
    };
    let authority_failure = Arc::new(Mutex::new(None));
    let progress_authority_failure = Arc::clone(&authority_failure);
    if let Err(error) = connection.progress_handler(
        MIGRATION_SQL_PROGRESS_INTERVAL_OPS,
        Some(move || {
            if shutdown_requested
                .as_ref()
                .is_some_and(|shutdown| shutdown.load(Ordering::Acquire))
                || deadline.is_some_and(|deadline| Instant::now() >= deadline)
            {
                return true;
            }
            if let Some((authority, intent)) = repeated_authority.as_ref()
                && let Err(error) = authority.verify(*intent)
            {
                *progress_authority_failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                return true;
            }
            false
        }),
    ) {
        let _ = connection.update_hook(None::<fn(Action, &str, &str, i64)>);
        let _ = connection.authorizer(Some(canonical_authorizer));
        return Err(sqlite_error("install execution guard", error));
    }

    let result = catch_unwind(AssertUnwindSafe(operation));
    let clear_progress =
        connection.progress_handler(MIGRATION_SQL_PROGRESS_INTERVAL_OPS, None::<fn() -> bool>);
    let clear_update_hook = connection.update_hook(None::<fn(Action, &str, &str, i64)>);
    let restore_authorizer = connection.authorizer(Some(canonical_authorizer));
    let cleanup = clear_progress
        .map_err(|error| sqlite_error("clear execution guard", error))
        .and_then(|()| {
            clear_update_hook.map_err(|error| sqlite_error("clear insert tracker", error))
        })
        .and_then(|()| {
            restore_authorizer.map_err(|error| sqlite_error("restore connection authorizer", error))
        });
    let result = match result {
        Ok(result) => result,
        Err(payload) => {
            let _ = cleanup;
            resume_unwind(payload);
        }
    };
    cleanup?;
    let authority_error = authority_failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(error) = authority_error {
        Err(error)
    } else if denied.load(Ordering::Acquire) {
        Err(MigrationSqlError::TransactionControlDenied)
    } else {
        result
    }
}

/// Authorizes the migration writer channel.
///
/// This channel legitimately builds durable schema, so ordinary `CREATE
/// TABLE` / `CREATE TRIGGER` is allowed. Temporary **tables and indexes** are
/// allowed for the same reason and with strictly less reach: a temp object
/// lives in the connection's own `temp` schema, cannot alias or mutate
/// anything in `main`, and disappears with the connection. Denying them while
/// permitting durable DDL inverted the blast radius, and it left derived
/// per-connection scratch — the projection output-state cache — unable to
/// exist at all.
///
/// Temporary **triggers and views** stay denied. A temp trigger can fire on a
/// durable table and mutate it outside the invariant trigger contract, which
/// is exactly the authority this channel must not hand out; temp views have no
/// caller. `ATTACH`/`DETACH` are allowed only while the writer actor runs its
/// fixed attachment lifecycle operations; caller-provided SQL cannot enable
/// them. `load_extension`, unrecognized actions, and non-allowlisted pragmas
/// remain denied unconditionally.
fn authorize_migration_writer(
    context: rusqlite::hooks::AuthContext<'_>,
    database_operation: Option<&AuthorizedDatabaseOperation>,
) -> Authorization {
    match context.action {
        AuthAction::Attach { .. }
            if matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Attach | AuthorizedDatabaseOperation::Vacuum)
            ) =>
        {
            return Authorization::Allow;
        }
        // SQLite supplies a null authorizer filename when ATTACH binds its
        // filename parameter. rusqlite preserves that action as Unknown.
        AuthAction::Unknown {
            code,
            arg1: None,
            arg2: None,
        } if code == rusqlite::ffi::SQLITE_ATTACH
            && matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Attach | AuthorizedDatabaseOperation::Vacuum)
            ) =>
        {
            return Authorization::Allow;
        }
        AuthAction::Detach { database_name }
            if matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Detach(expected))
                    if database_name.eq_ignore_ascii_case(expected)
            ) || matches!(
                database_operation,
                Some(AuthorizedDatabaseOperation::Vacuum)
            ) =>
        {
            return Authorization::Allow;
        }
        _ => {}
    }
    if matches!(
        context.action,
        AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
            | AuthAction::Unknown { .. }
    ) || matches!(
        context.action,
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("load_extension")
    ) || matches!(
        context.action,
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        }
        if !is_allowed_migration_pragma(pragma_name, pragma_value)
    ) {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

fn is_allowed_migration_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    is_migration_read_pragma(pragma_name, pragma_value)
        || (pragma_value.is_none() && pragma_name.eq_ignore_ascii_case("shrink_memory"))
        || pragma_value.is_some_and(|value| {
            (pragma_name.eq_ignore_ascii_case("auto_vacuum")
                && (value.eq_ignore_ascii_case("incremental") || value == "2"))
                || (pragma_name.eq_ignore_ascii_case("foreign_keys")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("defer_foreign_keys")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("busy_timeout")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("incremental_vacuum")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("secure_delete")
                    && (value.eq_ignore_ascii_case("on") || value == "1"))
                || (pragma_name.eq_ignore_ascii_case("user_version")
                    && value.parse::<u32>().is_ok())
                || (pragma_name.eq_ignore_ascii_case("wal_autocheckpoint")
                    && value.parse::<u32>().is_ok())
        })
}

fn is_migration_read_pragma(pragma_name: &str, pragma_value: Option<&str>) -> bool {
    const ARGUMENT_SAFE: &[&str] = &[
        "foreign_key_check",
        "foreign_key_list",
        "index_info",
        "index_list",
        "index_xinfo",
        "integrity_check",
        "quick_check",
        "table_info",
        "table_list",
        "table_xinfo",
    ];
    const NO_ARGUMENT_ONLY: &[&str] = &[
        "application_id",
        "auto_vacuum",
        "busy_timeout",
        "cache_size",
        "collation_list",
        "compile_options",
        "data_version",
        "database_list",
        "defer_foreign_keys",
        "foreign_keys",
        "freelist_count",
        "function_list",
        "journal_mode",
        "mmap_size",
        "module_list",
        "page_count",
        "page_size",
        "pragma_list",
        "query_only",
        "recursive_triggers",
        "schema_version",
        "secure_delete",
        "synchronous",
        "temp_store",
        "user_version",
        "wal_autocheckpoint",
    ];

    ARGUMENT_SAFE
        .iter()
        .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate))
        || (pragma_value.is_none()
            && NO_ARGUMENT_ONLY
                .iter()
                .any(|candidate| pragma_name.eq_ignore_ascii_case(candidate)))
}

pub(crate) fn execute_query(
    connection: &Connection,
    request: MigrationSqlStatement,
) -> Result<MigrationSqlRows, MigrationSqlError> {
    request.validate()?;
    with_migration_guard(
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
    request: MigrationSqlStatement,
) -> Result<MigrationSqlRows, MigrationSqlError> {
    let mut statement = connection
        .prepare(&request.sql)
        .map_err(|error| sqlite_error("prepare query", error))?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let column_count = columns.len();
    let values = request
        .params
        .into_iter()
        .map(MigrationSqlValue::into_rusqlite);
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
        .ok_or(MigrationSqlError::QueryLimitExceeded)?;
    while let Some(row) = query
        .next()
        .map_err(|error| sqlite_error("advance query", error))?
    {
        if rows.len() >= MAX_QUERY_ROWS {
            return Err(MigrationSqlError::QueryLimitExceeded);
        }
        materialized_bytes = materialized_bytes
            .checked_add(ROW_ALLOCATION_OVERHEAD)
            .and_then(|total| {
                CELL_ALLOCATION_OVERHEAD
                    .checked_mul(column_count)
                    .and_then(|cells| total.checked_add(cells))
            })
            .ok_or(MigrationSqlError::QueryLimitExceeded)?;
        if materialized_bytes > MAX_QUERY_BYTES {
            return Err(MigrationSqlError::QueryLimitExceeded);
        }
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = MigrationSqlValue::from_rusqlite(
                row.get_ref(index)
                    .map_err(|error| sqlite_error("read query value", error))?,
            )?;
            materialized_bytes = materialized_bytes
                .checked_add(value.materialized_bytes())
                .ok_or(MigrationSqlError::QueryLimitExceeded)?;
            if materialized_bytes > MAX_QUERY_BYTES {
                return Err(MigrationSqlError::QueryLimitExceeded);
            }
            values.push(value);
        }
        rows.push(MigrationSqlRow { values });
    }
    Ok(MigrationSqlRows { columns, rows })
}

fn sqlite_error(operation: &'static str, error: rusqlite::Error) -> MigrationSqlError {
    let (code, extended_code) = match &error {
        rusqlite::Error::SqliteFailure(error, _) => {
            (Some(error.extended_code & 0xff), Some(error.extended_code))
        }
        _ => (None, None),
    };
    MigrationSqlError::Sqlite {
        operation,
        code,
        extended_code,
        message: error.to_string(),
    }
}

fn map_writer_send_error(
    error: tokio_mpsc::error::TrySendError<WriterCommand>,
) -> MigrationSqlError {
    match error {
        tokio_mpsc::error::TrySendError::Full(_) => MigrationSqlError::Busy,
        tokio_mpsc::error::TrySendError::Closed(_) => MigrationSqlError::WriterUnavailable,
    }
}

fn transaction_terminal_error(expired: &AtomicBool) -> MigrationSqlError {
    if expired.load(Ordering::Acquire) {
        MigrationSqlError::TransactionExpired
    } else {
        MigrationSqlError::TransactionClosed
    }
}

fn map_transaction_send_error(
    error: mpsc::TrySendError<TransactionCommand>,
    expired: &AtomicBool,
) -> MigrationSqlError {
    match error {
        mpsc::TrySendError::Full(_) => MigrationSqlError::Busy,
        mpsc::TrySendError::Disconnected(_) => transaction_terminal_error(expired),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };

    use rusqlite::Savepoint;
    use tempfile::TempDir;
    use tracedecay_domain::LocatorDigest;
    use tracedecay_store::{
        AdmissionConfigV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1,
        StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
    };

    use crate::{
        ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
        reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
    };

    use super::*;

    struct AtomicWriteAuthority(Arc<AtomicBool>);

    impl MigrationSqlWriteAuthority for AtomicWriteAuthority {
        fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            if self.0.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(MigrationSqlError::AuthorityDenied("revoked".to_owned()))
            }
        }
    }

    struct SlowSchemaAuthority {
        execute_batch_checks: AtomicUsize,
    }

    impl MigrationSqlWriteAuthority for SlowSchemaAuthority {
        fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            if intent == MigrationSqlWriteIntent::ExecuteBatch
                && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) < 3
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(())
        }
    }

    struct RevokeDuringSchemaStep {
        execute_batch_checks: AtomicUsize,
    }

    impl MigrationSqlWriteAuthority for RevokeDuringSchemaStep {
        fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            if intent == MigrationSqlWriteIntent::ExecuteBatch
                && self.execute_batch_checks.fetch_add(1, Ordering::AcqRel) >= 1
            {
                return Err(MigrationSqlError::AuthorityDenied(
                    "revoked during schema step".to_owned(),
                ));
            }
            Ok(())
        }
    }

    struct NoWrites;

    impl StorageOperationExecutor for NoWrites {
        fn execute(
            &mut self,
            _savepoint: &Savepoint<'_>,
            _payload: &RepositoryWritePayloadV1,
        ) -> rusqlite::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct NoReads;

    impl ReaderQueryExecutor for NoReads {
        fn execute_read(
            &mut self,
            _snapshot: &rusqlite::Transaction<'_>,
            _request: &RuntimeReadRequestV1,
        ) -> Result<RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1> {
            unreachable!("migration SQL queries bypass the closed product read executor")
        }
    }

    struct Fixture {
        _directory: TempDir,
        writer: PersistentWriter,
        readers: ReaderPool<NoReads>,
    }

    fn binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.migration-sql",
                "profile_id": "profile.migration-sql",
                "scope": { "kind": "project", "project_id": "project.migration-sql" }
            },
            "incarnation": 3,
            "authority_epoch": 11
        }))
        .unwrap()
    }

    fn locator(binding: &StoreRuntimeBindingV1, byte: char) -> VerifiedStoreLocatorV1 {
        VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(3).unwrap(),
            LocatorDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap(),
        )
    }

    fn fixture(writer_digest: char, reader_digest: char) -> Fixture {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("migration.sqlite3");
        rusqlite::Connection::open(&path).unwrap();
        let path = path.canonicalize().unwrap();
        let binding = binding();
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(
                binding.clone(),
                locator(&binding, writer_digest),
                path.clone(),
            )
            .unwrap(),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding.clone(), locator(&binding, reader_digest), path)
                .unwrap(),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .unwrap();
        Fixture {
            _directory: directory,
            writer,
            readers,
        }
    }

    fn statement(sql: &str, params: Vec<MigrationSqlValue>) -> MigrationSqlStatement {
        MigrationSqlStatement::new(sql.to_owned(), params).unwrap()
    }

    #[test]
    fn attach_rejects_different_verified_locators() {
        let fixture = fixture('a', 'b');

        let result = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers);

        assert!(matches!(result, Err(MigrationSqlError::AuthorityMismatch)));
    }

    #[test]
    fn attach_rejects_same_locator_bound_to_different_files() {
        let first = fixture('a', 'a');
        let second = fixture('a', 'a');

        let result = MigrationSqlHandle::attach(&first.writer, &second.readers);

        assert!(matches!(result, Err(MigrationSqlError::AuthorityMismatch)));
    }

    #[test]
    fn read_only_clone_cannot_recover_writer_authority() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let read_only = channel.read_only_clone();

        let error = read_only
            .execute_batch("CREATE TABLE forbidden (value INTEGER)".to_owned())
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::WriterUnavailable));
    }

    #[test]
    fn schema_migration_transaction_requires_attached_write_authority() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let error = match channel.begin_schema_migration_immediate() {
            Ok(_) => panic!("schema migration must require attached authority"),
            Err(error) => error,
        };

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
    }

    #[test]
    fn writer_actor_allows_only_product_schema_pragmas() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        for pragma in [
            "PRAGMA auto_vacuum = INCREMENTAL",
            "PRAGMA foreign_keys = ON",
            "PRAGMA defer_foreign_keys = ON",
            "PRAGMA secure_delete = ON",
            "PRAGMA user_version = 24",
        ] {
            channel
                .execute_batch(pragma.to_owned())
                .unwrap_or_else(|error| panic!("{pragma} must remain available: {error}"));
        }
        for pragma in [
            "PRAGMA auto_vacuum = NONE",
            "PRAGMA foreign_keys = OFF",
            "PRAGMA secure_delete = OFF",
            "PRAGMA writable_schema = ON",
        ] {
            let error = channel.execute_batch(pragma.to_owned()).unwrap_err();
            assert!(
                matches!(error, MigrationSqlError::Sqlite { .. }),
                "{pragma}: {error}"
            );
        }
    }

    /// The projection output-state cache is derived, per-connection scratch
    /// rebuilt from `observation_projection_provenance` whenever
    /// `PRAGMA data_version` moves. It must be able to exist on this channel;
    /// denying it made every projection version migration and rebuild fail.
    #[test]
    fn writer_actor_allows_temp_tables_and_indexes_but_not_temp_triggers_or_views() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE durable (value INTEGER NOT NULL)".to_owned())
            .expect("durable table remains available");

        // The exact shape the projection output-state cache creates.
        channel
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS observation_projection_output_state (
                    projector_version TEXT NOT NULL,
                    output_provider TEXT NOT NULL,
                    output_message_id TEXT NOT NULL,
                    canonical_observation_id TEXT NOT NULL,
                    latest_observation_id TEXT NOT NULL,
                    latest_sequence INTEGER NOT NULL CHECK(latest_sequence >= 0),
                    projector_owned INTEGER NOT NULL CHECK(projector_owned IN (0, 1)),
                    owner_count INTEGER NOT NULL CHECK(owner_count > 0),
                    PRIMARY KEY(projector_version, output_provider, output_message_id)
                 ) WITHOUT ROWID;"
                    .to_owned(),
            )
            .expect("projection output-state cache must be creatable");
        channel
            .execute_batch(
                "CREATE TEMP TABLE scratch (value INTEGER NOT NULL);
                 CREATE INDEX temp.scratch_value ON scratch(value);
                 INSERT INTO temp.scratch(value) VALUES (1);
                 DELETE FROM temp.scratch;
                 DROP INDEX temp.scratch_value;
                 DROP TABLE temp.scratch;"
                    .to_owned(),
            )
            .expect("temp scratch must be creatable, writable, and droppable");

        // A temp trigger could mutate durable rows outside the invariant
        // trigger contract, and a temp view has no caller: both stay denied.
        for denied in [
            "CREATE TEMP TRIGGER durable_guard AFTER INSERT ON durable
             BEGIN DELETE FROM durable; END",
            "CREATE TEMP VIEW durable_view AS SELECT value FROM durable",
        ] {
            let error = channel
                .execute_batch(denied.to_owned())
                .expect_err("temp triggers and views must stay denied");
            assert!(
                matches!(error, MigrationSqlError::Sqlite { .. }),
                "{denied}: {error}"
            );
        }
    }

    #[test]
    fn writer_checkpoint_returns_status() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let rows = channel.checkpoint_wal_truncate().unwrap();

        assert_eq!(rows.columns.len(), 3);
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0].values.len(), 3);
    }

    #[test]
    fn ordinary_transaction_cannot_request_an_unbounded_schema_step() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(
                true,
            )))))
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();

        let error = transaction
            .execute_schema_batch_step("CREATE TABLE forbidden_schema_mode (id INTEGER)".to_owned())
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
        transaction.rollback().unwrap();
    }

    #[test]
    fn schema_migration_renews_its_lease_after_successful_bounded_steps() {
        let fixture = fixture('a', 'a');
        let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE lease_probe (value INTEGER)".to_owned())
            .unwrap();
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(
                true,
            )))))
            .unwrap();
        let transaction = channel.begin_schema_migration_immediate().unwrap();
        let started = Instant::now();

        for value in 0..5 {
            std::thread::sleep(Duration::from_millis(125));
            transaction
                .execute(statement(
                    "INSERT INTO lease_probe VALUES (?)",
                    vec![MigrationSqlValue::Integer(value)],
                ))
                .unwrap();
        }
        assert!(started.elapsed() > MIGRATION_SQL_TRANSACTION_LIMIT);
        transaction.commit().unwrap();

        let rows = base
            .query(
                statement("SELECT count(*) FROM lease_probe", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(5)]);
    }

    #[test]
    fn explicit_schema_step_has_no_guessed_deadline_and_rechecks_authority() {
        let fixture = fixture('a', 'a');
        let authority = Arc::new(SlowSchemaAuthority {
            execute_batch_checks: AtomicUsize::new(0),
        });
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(authority.clone())
            .unwrap();
        let transaction = channel.begin_schema_migration_immediate().unwrap();
        let started = Instant::now();

        transaction
            .execute_schema_batch_step(
                "CREATE TABLE long_schema_step (value INTEGER);
                 WITH RECURSIVE n(value) AS (
                     VALUES(1)
                     UNION ALL
                     SELECT value + 1 FROM n WHERE value < 10000
                 )
                 INSERT INTO long_schema_step SELECT value FROM n;"
                    .to_owned(),
            )
            .unwrap();

        assert!(started.elapsed() > MIGRATION_SQL_EXECUTION_LIMIT);
        assert!(authority.execute_batch_checks.load(Ordering::Acquire) > 3);
        transaction.rollback().unwrap();
    }

    #[test]
    fn authority_loss_during_schema_step_rolls_back_transaction() {
        let fixture = fixture('a', 'a');
        let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(Arc::new(RevokeDuringSchemaStep {
                execute_batch_checks: AtomicUsize::new(0),
            }))
            .unwrap();
        let transaction = channel.begin_schema_migration_immediate().unwrap();

        let error = transaction
            .execute_schema_batch_step(
                "CREATE TABLE revoked_schema_step (value INTEGER);
                 WITH RECURSIVE n(value) AS (
                     VALUES(1)
                     UNION ALL
                     SELECT value + 1 FROM n WHERE value < 10000
                 )
                 INSERT INTO revoked_schema_step SELECT value FROM n;"
                    .to_owned(),
            )
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
        let rows = base
            .query(
                statement(
                    "SELECT count(*) FROM sqlite_schema WHERE name = 'revoked_schema_step'",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn execute_batch_execute_and_query_use_owned_dtos() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch(
                "CREATE TABLE migrated (
                    id INTEGER PRIMARY KEY,
                    score REAL,
                    label TEXT,
                    payload BLOB,
                    optional TEXT
                )"
                .to_owned(),
            )
            .unwrap();

        let executed = channel
            .execute(statement(
                "INSERT INTO migrated VALUES (?, ?, ?, ?, ?)",
                vec![
                    MigrationSqlValue::Integer(7),
                    MigrationSqlValue::Real(2.5),
                    MigrationSqlValue::Text("owned".to_owned()),
                    MigrationSqlValue::Blob(vec![1, 2, 3]),
                    MigrationSqlValue::Null,
                ],
            ))
            .unwrap();
        let rows = channel
            .query(
                statement(
                    "SELECT id, score, label, payload, optional FROM migrated",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(executed.changed_rows, 1);
        assert_eq!(
            rows.columns,
            vec!["id", "score", "label", "payload", "optional"]
        );
        assert_eq!(
            rows.rows,
            vec![MigrationSqlRow {
                values: vec![
                    MigrationSqlValue::Integer(7),
                    MigrationSqlValue::Real(2.5),
                    MigrationSqlValue::Text("owned".to_owned()),
                    MigrationSqlValue::Blob(vec![1, 2, 3]),
                    MigrationSqlValue::Null,
                ],
            }]
        );
    }

    #[test]
    fn statement_admission_limits_accept_boundaries_and_reject_oversize() {
        assert!(MigrationSqlStatement::new("x".repeat(MAX_SQL_BYTES), vec![]).is_ok());
        assert!(matches!(
            MigrationSqlStatement::new("x".repeat(MAX_SQL_BYTES + 1), vec![]),
            Err(MigrationSqlError::RequestLimitExceeded)
        ));
        assert!(
            MigrationSqlStatement::new(
                "SELECT 1".to_owned(),
                vec![MigrationSqlValue::Null; MAX_SQL_PARAMETERS],
            )
            .is_ok()
        );
        assert!(matches!(
            MigrationSqlStatement::new(
                "SELECT 1".to_owned(),
                vec![MigrationSqlValue::Null; MAX_SQL_PARAMETERS + 1],
            ),
            Err(MigrationSqlError::RequestLimitExceeded)
        ));
    }

    #[test]
    fn batch_admission_rejects_oversize_before_enqueue() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let error = channel
            .execute_batch("x".repeat(MAX_SQL_BYTES + 1))
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::RequestLimitExceeded));
    }

    #[test]
    fn validate_checks_syntax_and_schema_on_the_writer_actor() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let missing = channel
            .validate(statement("SELECT value FROM missing_table", vec![]))
            .unwrap_err();
        let syntax = channel
            .validate(statement("SELECT FROM", vec![]))
            .unwrap_err();

        assert!(matches!(missing, MigrationSqlError::Sqlite { .. }));
        assert!(matches!(syntax, MigrationSqlError::Sqlite { .. }));
    }

    #[test]
    fn batch_reports_last_insert_rowid() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch(
                "CREATE TABLE batch_id (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    value TEXT NOT NULL
                )"
                .to_owned(),
            )
            .unwrap();

        let result = channel
            .execute_batch(
                "INSERT INTO batch_id(value) VALUES ('first');
                 INSERT INTO batch_id(value) VALUES ('second');"
                    .to_owned(),
            )
            .unwrap();

        assert_eq!(result.changed_rows, 2);
        assert_eq!(result.last_insert_rowid, 2);
        assert_eq!(channel.last_insert_rowid(), 2);
    }

    #[test]
    fn rowid_is_handle_local_and_changes_only_after_applied_insert() {
        let fixture = fixture('a', 'a');
        let channel_a = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let channel_b = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel_a
            .execute_batch(
                "CREATE TABLE rowids (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL UNIQUE
                )"
                .to_owned(),
            )
            .unwrap();

        let a = channel_a
            .execute(statement(
                "INSERT INTO rowids(value) VALUES (?)",
                vec![MigrationSqlValue::Text("a".to_owned())],
            ))
            .unwrap();
        let b = channel_b
            .execute(statement(
                "INSERT INTO rowids(value) VALUES (?)",
                vec![MigrationSqlValue::Text("b".to_owned())],
            ))
            .unwrap();
        assert_eq!(a.last_insert_rowid, 1);
        assert_eq!(b.last_insert_rowid, 2);

        let update = channel_a
            .execute(statement(
                "UPDATE rowids SET value = ? WHERE id = 1",
                vec![MigrationSqlValue::Text("updated".to_owned())],
            ))
            .unwrap();
        channel_a
            .validate(statement("SELECT value FROM rowids", vec![]))
            .unwrap();
        channel_a
            .query(
                statement("SELECT value FROM rowids WHERE id = 1", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        let ignored = channel_a
            .execute(statement(
                "INSERT OR IGNORE INTO rowids(value) VALUES (?)",
                vec![MigrationSqlValue::Text("b".to_owned())],
            ))
            .unwrap();
        let upsert_update = channel_a
            .execute(statement(
                "INSERT INTO rowids(id, value) VALUES (2, 'b')
                 ON CONFLICT(id) DO UPDATE SET value = excluded.value",
                vec![],
            ))
            .unwrap();

        assert_eq!(update.last_insert_rowid, 1);
        assert_eq!(ignored.last_insert_rowid, 1);
        assert_eq!(upsert_update.last_insert_rowid, 1);
        assert_eq!(channel_a.last_insert_rowid(), 1);
        assert_eq!(channel_b.last_insert_rowid(), 2);

        let explicit = channel_a
            .execute(statement(
                "INSERT INTO rowids(id, value) VALUES (?, ?)",
                vec![
                    MigrationSqlValue::Integer(41),
                    MigrationSqlValue::Text("explicit".to_owned()),
                ],
            ))
            .unwrap();
        assert_eq!(explicit.last_insert_rowid, 41);
        assert_eq!(channel_a.last_insert_rowid(), 41);
    }

    #[test]
    fn partial_batch_error_still_publishes_applied_insert_rowid() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch(
                "CREATE TABLE partial_rowid (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                )"
                .to_owned(),
            )
            .unwrap();

        let error = channel
            .execute_batch(
                "INSERT INTO partial_rowid(value) VALUES ('autocommit');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                    .to_owned(),
            )
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::Sqlite { .. }));
        assert_eq!(channel.last_insert_rowid(), 1);

        let transaction = channel.begin_immediate().unwrap();
        let error = transaction
            .execute_batch(
                "INSERT INTO partial_rowid(value) VALUES ('pinned');
                 INSERT INTO missing_table(value) VALUES ('fail');"
                    .to_owned(),
            )
            .unwrap_err();
        assert!(matches!(error, MigrationSqlError::Sqlite { .. }));
        assert_eq!(channel.last_insert_rowid(), 2);
        transaction.rollback().unwrap();
        assert_eq!(channel.last_insert_rowid(), 2);
    }

    #[test]
    fn transaction_insert_returning_publishes_rowid() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch(
                "CREATE TABLE returning_rowid (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                )"
                .to_owned(),
            )
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();

        let rows = transaction
            .query(statement(
                "INSERT INTO returning_rowid(value) VALUES ('value') RETURNING id",
                vec![],
            ))
            .unwrap();

        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
        assert_eq!(channel.last_insert_rowid(), 1);
        transaction.rollback().unwrap();
    }

    #[test]
    fn deferred_transaction_is_available_for_default_sqlite_semantics() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE deferred (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_deferred().unwrap();
        transaction
            .execute(statement(
                "INSERT INTO deferred VALUES (?)",
                vec![MigrationSqlValue::Integer(1)],
            ))
            .unwrap();

        transaction.commit().unwrap();

        let rows = channel
            .query(
                statement("SELECT count(*) FROM deferred", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
    }

    #[test]
    fn immediate_transaction_commit_reports_only_after_commit() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE committed (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();
        transaction
            .execute(statement(
                "INSERT INTO committed VALUES (?)",
                vec![MigrationSqlValue::Integer(41)],
            ))
            .unwrap();
        let inside = transaction
            .query(statement("SELECT value FROM committed", vec![]))
            .unwrap();

        assert_eq!(inside.rows[0].values, vec![MigrationSqlValue::Integer(41)]);
        let receipt = transaction.commit().unwrap();
        assert_eq!(receipt.changed_rows, 1);
        let committed = channel
            .query(
                statement("SELECT value FROM committed", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            committed.rows[0].values,
            vec![MigrationSqlValue::Integer(41)]
        );
    }

    #[test]
    fn transaction_attachment_is_exact_and_auto_detached() {
        let fixture = fixture('a', 'a');
        let source_path = fixture._directory.path().join("source.sqlite3");
        rusqlite::Connection::open(&source_path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE source_rows(value INTEGER NOT NULL);
                 INSERT INTO source_rows VALUES (7);",
            )
            .unwrap();
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let attachment =
            || MigrationSqlAttachment::new(source_path.to_string_lossy(), "source_input").unwrap();

        let transaction = channel.begin_immediate().unwrap();
        transaction.attach_database(attachment()).unwrap();
        let rows = transaction
            .query(statement(
                "SELECT value FROM source_input.source_rows",
                vec![],
            ))
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(7)]);
        transaction.commit().unwrap();

        let transaction = channel.begin_immediate().unwrap();
        transaction
            .attach_database(attachment())
            .expect("commit must detach the prior exact input");
        transaction.rollback().unwrap();

        let transaction = channel.begin_immediate().unwrap();
        transaction
            .attach_database(attachment())
            .expect("rollback must detach the prior exact input");
        drop(transaction);

        let transaction = channel.begin_immediate().unwrap();
        transaction
            .attach_database(attachment())
            .expect("dropping a transaction must detach the prior exact input");
        transaction.rollback().unwrap();
    }

    #[test]
    fn caller_sql_cannot_attach_database() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        channel
            .execute(statement(
                "ATTACH DATABASE ?1 AS caller_input",
                vec![MigrationSqlValue::Text(":memory:".to_owned())],
            ))
            .unwrap_err();
        let databases = channel
            .query(
                statement("PRAGMA database_list", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert!(databases.rows.iter().all(|row| {
            !matches!(
                row.values.get(1),
                Some(MigrationSqlValue::Text(name)) if name == "caller_input"
            )
        }));
    }

    #[test]
    fn immediate_transaction_rollback_reports_after_rollback_and_discards_rows() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE rolled_back (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();
        transaction
            .execute(statement(
                "INSERT INTO rolled_back VALUES (?)",
                vec![MigrationSqlValue::Integer(99)],
            ))
            .unwrap();

        let receipt = transaction.rollback().unwrap();

        assert_eq!(receipt.discarded_changed_rows, 1);
        let rows = channel
            .query(
                statement("SELECT count(*) FROM rolled_back", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn pinned_batch_rejects_transaction_control() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let transaction = channel.begin_immediate().unwrap();

        let error = transaction
            .execute_batch("COMMIT; BEGIN IMMEDIATE".to_owned())
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
        transaction.rollback().unwrap();
    }

    #[test]
    fn pinned_execute_rejects_transaction_control_before_commit_receipt() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let transaction = channel.begin_immediate().unwrap();

        let error = transaction
            .execute(statement("COMMIT", vec![]))
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
        transaction.rollback().unwrap();
    }

    #[test]
    fn unpinned_batch_rejects_transaction_control_and_releases_writer() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let error = channel
            .execute_batch("BEGIN IMMEDIATE".to_owned())
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::TransactionControlDenied));
        channel
            .execute_batch("CREATE TABLE after_denied_begin (value INTEGER)".to_owned())
            .unwrap();
    }

    #[test]
    fn mutating_no_argument_pragmas_are_denied() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        for pragma in [
            "PRAGMA cache_flush",
            "PRAGMA incremental_vacuum",
            "PRAGMA optimize",
            "PRAGMA wal_checkpoint",
        ] {
            let error = channel.execute_batch(pragma.to_owned()).unwrap_err();
            assert!(
                matches!(
                    error,
                    MigrationSqlError::Sqlite {
                        code: Some(23),
                        extended_code: Some(23),
                        ..
                    }
                ),
                "{pragma} must be denied, got {error:?}"
            );
        }
    }

    #[test]
    fn connection_local_memory_release_pragma_is_allowed() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        channel
            .execute_batch("PRAGMA shrink_memory".to_owned())
            .expect("connection-local cache release must be authorized");
    }

    #[test]
    fn migration_read_policy_allows_integrity_diagnostic_arguments() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE pragma_probe (value INTEGER)".to_owned())
            .unwrap();
        let transaction = channel.begin_deferred().unwrap();
        for pragma in [
            "PRAGMA quick_check",
            "PRAGMA quick_check(1000)",
            "PRAGMA integrity_check",
            "PRAGMA integrity_check(1000)",
        ] {
            let rows = transaction.query(statement(pragma, vec![])).unwrap();
            assert_eq!(
                rows.rows[0].values,
                vec![MigrationSqlValue::Text("ok".to_owned())],
                "{pragma} must remain classified as a read-only diagnostic"
            );
        }
        let table_info = transaction
            .query(statement("PRAGMA table_info(pragma_probe)", vec![]))
            .unwrap();
        assert_eq!(
            table_info.rows[0].values[1],
            MigrationSqlValue::Text("value".to_owned())
        );
        transaction.rollback().unwrap();
    }

    #[test]
    fn queued_write_rechecks_authority_on_actor_dequeue() {
        let fixture = fixture('a', 'a');
        let holder = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let transaction = holder.begin_immediate().unwrap();
        let (reply, receive) = std::sync::mpsc::sync_channel(1);
        assert!(
            holder
                .writer
                .as_ref()
                .unwrap()
                .try_send(WriterCommand::Dispatch {
                    request: MigrationSqlRequest::ExecuteBatch(
                        "CREATE TABLE denied_after_queue (value INTEGER)".to_owned(),
                    ),
                    reply,
                    last_insert_rowid: Arc::new(AtomicI64::new(0)),
                    authority: Some(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed)))),
                })
                .is_ok()
        );

        allowed.store(false, Ordering::Release);
        transaction.rollback().unwrap();
        let error = receive
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
        let rows = holder
            .query(
                statement(
                    "SELECT count(*) FROM sqlite_schema
                     WHERE name = 'denied_after_queue'",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn revoked_commit_rolls_back_pinned_transaction() {
        let fixture = fixture('a', 'a');
        let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE denied_commit (value INTEGER)".to_owned())
            .unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let guarded = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed))))
            .unwrap();
        let transaction = guarded.begin_immediate().unwrap();
        transaction
            .execute(statement("INSERT INTO denied_commit VALUES (1)", vec![]))
            .unwrap();

        allowed.store(false, Ordering::Release);
        let error = transaction.commit().unwrap_err();

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
        let rows = base
            .query(
                statement("SELECT count(*) FROM denied_commit", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn revoked_pinned_dispatch_rolls_back_and_releases_writer() {
        let fixture = fixture('a', 'a');
        let base = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        base.execute_batch("CREATE TABLE denied_dispatch (value INTEGER)".to_owned())
            .unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let guarded = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers)
            .unwrap()
            .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::clone(&allowed))))
            .unwrap();
        let transaction = guarded.begin_immediate().unwrap();
        transaction
            .execute(statement("INSERT INTO denied_dispatch VALUES (1)", vec![]))
            .unwrap();

        allowed.store(false, Ordering::Release);
        let error = transaction
            .execute(statement("INSERT INTO denied_dispatch VALUES (2)", vec![]))
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::AuthorityDenied(_)));
        let rows = base
            .query(
                statement("SELECT count(*) FROM denied_dispatch", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn pinned_batch_allows_named_savepoint_rollback_and_release() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE savepoint_value (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();

        transaction
            .execute_batch(
                "SAVEPOINT projection_collision_guard;
                 INSERT INTO savepoint_value VALUES (1);
                 ROLLBACK TO projection_collision_guard;
                 RELEASE projection_collision_guard;"
                    .to_owned(),
            )
            .unwrap();
        transaction.commit().unwrap();

        let rows = channel
            .query(
                statement("SELECT count(*) FROM savepoint_value", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn pinned_batch_allows_schema_migration_ddl() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE old_name (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();

        transaction
            .execute_batch(
                "ALTER TABLE old_name RENAME TO new_name;
                 CREATE INDEX new_name_value ON new_name(value);
                 DROP INDEX new_name_value;"
                    .to_owned(),
            )
            .unwrap();
        transaction.commit().unwrap();

        let rows = channel
            .query(
                statement(
                    "SELECT count(*) FROM sqlite_schema WHERE name = 'new_name'",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
    }

    #[test]
    fn unpinned_batch_allows_schema_migration_ddl() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE protected (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        let transaction = channel.begin_immediate().unwrap();
        transaction
            .execute_batch("INSERT INTO protected VALUES (1)".to_owned())
            .unwrap();
        transaction.commit().unwrap();

        channel
            .execute_batch("DROP TABLE protected".to_owned())
            .unwrap();
    }

    #[test]
    fn migration_guard_restores_authorizer_after_success() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .authorizer(Some(crate::connection::authorize_writer))
            .unwrap();
        connection
            .execute_batch("CREATE TABLE protected (value INTEGER)")
            .unwrap();

        with_migration_guard(
            &connection,
            false,
            false,
            None,
            None,
            true,
            None,
            crate::connection::authorize_writer,
            true,
            None,
            None,
            || {
                connection
                    .execute_batch("DROP TABLE protected")
                    .map_err(|error| sqlite_error("test migration DDL", error))
            },
        )
        .unwrap();

        connection
            .execute_batch("CREATE TABLE protected (value INTEGER)")
            .unwrap();
        assert!(connection.execute_batch("DROP TABLE protected").is_err());
    }

    #[test]
    fn migration_guard_restores_authorizer_after_panic() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .authorizer(Some(crate::connection::authorize_writer))
            .unwrap();

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), MigrationSqlError> = with_migration_guard(
                &connection,
                false,
                false,
                None,
                None,
                true,
                None,
                crate::connection::authorize_writer,
                true,
                None,
                None,
                || panic!("migration operation panic"),
            );
        }));

        assert!(panic.is_err());
        std::thread::sleep(MIGRATION_SQL_EXECUTION_LIMIT + Duration::from_millis(50));
        let sum: i64 = connection
            .query_row(
                "WITH RECURSIVE n(value) AS (
                    VALUES(1)
                    UNION ALL
                    SELECT value + 1 FROM n WHERE value < 2000
                 )
                 SELECT sum(value) FROM n",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sum, 2_001_000);
        connection
            .execute_batch("CREATE TABLE protected (value INTEGER)")
            .unwrap();
        assert!(connection.execute_batch("DROP TABLE protected").is_err());
    }

    #[test]
    fn dropping_pinned_transaction_rolls_back() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE dropped (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        {
            let transaction = channel.begin_immediate().unwrap();
            transaction
                .execute(statement(
                    "INSERT INTO dropped VALUES (?)",
                    vec![MigrationSqlValue::Integer(8)],
                ))
                .unwrap();
        }

        let rows = channel
            .query(
                statement("SELECT count(*) FROM dropped", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(rows.rows[0].values, vec![MigrationSqlValue::Integer(0)]);
    }

    #[test]
    fn writer_shutdown_rolls_back_and_closes_a_leaked_transaction() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let transaction = channel.begin_immediate().unwrap();
        let Fixture {
            _directory,
            writer,
            readers,
        } = fixture;
        let (finished, receive) = std::sync::mpsc::sync_channel(1);

        std::thread::spawn(move || {
            drop(writer);
            let _ = finished.send(());
        });

        receive
            .recv_timeout(Duration::from_secs(1))
            .expect("writer shutdown must not wait forever on leaked migration transaction");
        assert!(matches!(
            transaction.commit(),
            Err(MigrationSqlError::TransactionClosed)
        ));
        drop(readers);
        drop(_directory);
    }

    #[test]
    fn idle_transaction_expires_and_releases_writer() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let transaction = channel.begin_immediate().unwrap();

        std::thread::sleep(MIGRATION_SQL_TRANSACTION_IDLE_LIMIT + Duration::from_millis(100));

        assert!(matches!(
            transaction.commit(),
            Err(MigrationSqlError::TransactionExpired)
        ));
        channel
            .execute_batch("CREATE TABLE after_idle_expiry (value INTEGER)".to_owned())
            .unwrap();
    }

    #[test]
    fn active_transaction_hits_absolute_lease_and_releases_writer() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let transaction = channel.begin_immediate().unwrap();
        let started = Instant::now();

        let error = loop {
            match transaction.query(statement("SELECT 1", vec![])) {
                Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => break error,
            }
        };

        assert!(matches!(error, MigrationSqlError::TransactionExpired));
        assert!(started.elapsed() < Duration::from_secs(2));
        channel
            .execute_batch("CREATE TABLE after_absolute_expiry (value INTEGER)".to_owned())
            .unwrap();
    }

    #[test]
    fn query_materialization_is_bounded() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let error = channel
            .query(
                statement(
                    "WITH RECURSIVE n(value) AS (
                        VALUES(1)
                        UNION ALL
                        SELECT value + 1 FROM n WHERE value <= 10000
                    )
                    SELECT value FROM n",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert!(matches!(error, MigrationSqlError::QueryLimitExceeded));
    }

    #[test]
    fn query_execution_time_is_bounded() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        let started = Instant::now();

        let error = channel
            .query(
                statement(
                    "WITH RECURSIVE n(value) AS (
                        VALUES(1)
                        UNION ALL
                        SELECT value + 1 FROM n WHERE value < 100000
                    )
                    SELECT count(*) FROM n AS left_n CROSS JOIN n AS right_n",
                    vec![],
                ),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            MigrationSqlError::Sqlite { code: Some(9), .. }
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn invalid_sqlite_text_is_rejected_without_lossy_conversion() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch(
                "CREATE TABLE invalid_text (value TEXT NOT NULL);
                 INSERT INTO invalid_text(value) VALUES (CAST(x'80' AS TEXT));"
                    .to_owned(),
            )
            .unwrap();

        let error = channel
            .query(
                statement("SELECT value FROM invalid_text", vec![]),
                Duration::from_secs(1),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            MigrationSqlError::Sqlite {
                operation: "decode query text",
                ..
            }
        ));
    }

    #[test]
    fn sqlite_errors_preserve_primary_and_extended_codes() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE unique_value (value INTEGER UNIQUE)".to_owned())
            .unwrap();
        channel
            .execute(statement(
                "INSERT INTO unique_value VALUES (?)",
                vec![MigrationSqlValue::Integer(1)],
            ))
            .unwrap();

        let error = channel
            .execute(statement(
                "INSERT INTO unique_value VALUES (?)",
                vec![MigrationSqlValue::Integer(1)],
            ))
            .unwrap_err();

        assert!(matches!(
            error,
            MigrationSqlError::Sqlite {
                code: Some(19),
                extended_code: Some(2067),
                ..
            }
        ));
    }

    #[test]
    fn read_snapshot_stays_frozen_across_queries() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();
        channel
            .execute_batch("CREATE TABLE frozen (value INTEGER NOT NULL)".to_owned())
            .unwrap();
        channel
            .execute(statement(
                "INSERT INTO frozen VALUES (?)",
                vec![MigrationSqlValue::Integer(1)],
            ))
            .unwrap();
        let snapshot = channel.begin_read_snapshot(Duration::from_secs(1)).unwrap();
        let first = snapshot
            .query(statement("SELECT count(*) FROM frozen", vec![]))
            .unwrap();
        channel
            .execute(statement(
                "INSERT INTO frozen VALUES (?)",
                vec![MigrationSqlValue::Integer(2)],
            ))
            .unwrap();

        let frozen = snapshot
            .query(statement("SELECT count(*) FROM frozen", vec![]))
            .unwrap();

        assert_eq!(first.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
        assert_eq!(frozen.rows[0].values, vec![MigrationSqlValue::Integer(1)]);
        drop(snapshot);
        let current = channel
            .query(
                statement("SELECT count(*) FROM frozen", vec![]),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(current.rows[0].values, vec![MigrationSqlValue::Integer(2)]);
    }

    #[test]
    fn health_snapshot_retires_the_reserved_reader() {
        let fixture = fixture('a', 'a');
        let channel = MigrationSqlHandle::attach(&fixture.writer, &fixture.readers).unwrap();

        let snapshot = channel
            .begin_health_read_snapshot(Duration::from_secs(1))
            .unwrap();
        assert_eq!(fixture.readers.snapshot().leased_health, 1);
        drop(snapshot);

        let pool = fixture.readers.snapshot();
        assert_eq!(pool.leased_health, 0);
        assert_eq!(pool.health_workers, 0);
    }
}
