//! The owned vocabulary the migration transport speaks in.
//!
//! Every type here is a value: no SQLite connection, statement, or filesystem
//! path crosses this boundary, which is what lets the transport hand results to
//! callers that hold no store authority.

use std::error::Error;
use std::fmt;

use rusqlite::types::{Value, ValueRef};

use super::{CELL_ALLOCATION_OVERHEAD, MAX_REQUEST_BYTES, MAX_SQL_BYTES, MAX_SQL_PARAMETERS};

#[derive(Clone, Debug, PartialEq)]
pub enum MigrationSqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl MigrationSqlValue {
    pub(super) fn into_rusqlite(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(value) => Value::Integer(value),
            Self::Real(value) => Value::Real(value),
            Self::Text(value) => Value::Text(value),
            Self::Blob(value) => Value::Blob(value),
        }
    }

    pub(super) fn from_rusqlite(value: ValueRef<'_>) -> Result<Self, MigrationSqlError> {
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

    pub(super) fn materialized_bytes(&self) -> usize {
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

    pub(super) fn validate(&self) -> Result<(), MigrationSqlError> {
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

pub(super) fn valid_database_name(value: &str) -> bool {
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
    pub(super) fn intent(&self) -> MigrationSqlWriteIntent {
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

/// How long a write transaction is allowed to hold the writer.
///
/// `Ordinary` is the default for every mutation: one fixed lease, no renewal.
/// `AuthorizedLongLease` exists for the three write shapes that legitimately
/// outrun a single lease while continuously making progress — fresh-schema
/// installation, real-scale index installation, and full-index bulk
/// replacement. None of them steps an existing store forward from an older
/// shape; there is no version ladder behind this policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MigrationSqlTransactionPolicy {
    Ordinary,
    AuthorizedLongLease,
}

/// Whether one statement inside a transaction carries the ordinary
/// per-statement deadline.
///
/// `AuthorizedLongSchema` is accepted only inside an
/// [`MigrationSqlTransactionPolicy::AuthorizedLongLease`] transaction, and only
/// for durable schema DDL: a single `CREATE INDEX` over a real-scale table can
/// exceed the ordinary statement deadline while still being one bounded,
/// cancellable unit of work.
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
