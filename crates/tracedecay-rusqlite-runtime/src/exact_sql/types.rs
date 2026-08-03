//! The owned vocabulary the exact SQL transport speaks in.
//!
//! Every type here is a value: no SQLite connection, statement, or filesystem
//! path crosses this boundary, which is what lets the transport hand results to
//! callers that hold no store authority.

use std::error::Error;
use std::fmt;

use rusqlite::types::{Value, ValueRef};

use super::{CELL_ALLOCATION_OVERHEAD, MAX_REQUEST_BYTES, MAX_SQL_BYTES, MAX_SQL_PARAMETERS};

#[derive(Clone, Debug, PartialEq)]
pub enum ExactSqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl ExactSqlValue {
    pub(super) fn into_rusqlite(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(value) => Value::Integer(value),
            Self::Real(value) => Value::Real(value),
            Self::Text(value) => Value::Text(value),
            Self::Blob(value) => Value::Blob(value),
        }
    }

    pub(super) fn from_rusqlite(value: ValueRef<'_>) -> Result<Self, ExactSqlError> {
        Ok(match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value) => Self::Integer(value),
            ValueRef::Real(value) => Self::Real(value),
            ValueRef::Text(value) => Self::Text(
                std::str::from_utf8(value)
                    .map_err(|error| ExactSqlError::Sqlite {
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
pub struct ExactSqlStatement {
    pub sql: String,
    pub params: Vec<ExactSqlValue>,
}

impl ExactSqlStatement {
    pub fn new(sql: String, params: Vec<ExactSqlValue>) -> Result<Self, ExactSqlError> {
        let statement = Self { sql, params };
        statement.validate()?;
        Ok(statement)
    }

    pub(super) fn validate(&self) -> Result<(), ExactSqlError> {
        if self.sql.trim().is_empty() {
            return Err(ExactSqlError::InvalidStatement);
        }
        if self.sql.capacity() > MAX_SQL_BYTES || self.params.capacity() > MAX_SQL_PARAMETERS {
            return Err(ExactSqlError::RequestLimitExceeded);
        }
        let bytes = CELL_ALLOCATION_OVERHEAD
            .checked_mul(self.params.capacity())
            .and_then(|params| self.sql.capacity().checked_add(params))
            .and_then(|initial| {
                self.params.iter().try_fold(initial, |total, value| {
                    let retained = match value {
                        ExactSqlValue::Text(value) => value.capacity(),
                        ExactSqlValue::Blob(value) => value.capacity(),
                        _ => value.materialized_bytes(),
                    };
                    total.checked_add(retained)
                })
            });
        if bytes.is_none_or(|bytes| bytes > MAX_REQUEST_BYTES) {
            return Err(ExactSqlError::RequestLimitExceeded);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactSqlAttachment {
    filename: String,
    database_name: String,
}

impl ExactSqlAttachment {
    pub fn new(
        filename: impl Into<String>,
        database_name: impl Into<String>,
    ) -> Result<Self, ExactSqlError> {
        let filename = filename.into();
        let database_name = database_name.into();
        if filename.is_empty()
            || filename.len() > MAX_SQL_BYTES
            || !valid_database_name(&database_name)
        {
            return Err(ExactSqlError::InvalidAttachment);
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
pub struct ExactSqlRow {
    pub values: Vec<ExactSqlValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExactSqlRows {
    pub columns: Vec<String>,
    pub rows: Vec<ExactSqlRow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSqlExecuteResult {
    pub changed_rows: usize,
    pub last_insert_rowid: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSqlBatchResult {
    pub changed_rows: u64,
    pub last_insert_rowid: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSqlCommitReceipt {
    pub changed_rows: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactSqlRollbackReceipt {
    pub discarded_changed_rows: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SqlRequest {
    Validate(ExactSqlStatement),
    Execute(ExactSqlStatement),
    Query(ExactSqlStatement),
    ExecuteBatch(String),
}

impl SqlRequest {
    pub(super) fn intent(&self) -> ExactSqlWriteIntent {
        match self {
            Self::Validate(_) => ExactSqlWriteIntent::Validate,
            Self::Execute(_) => ExactSqlWriteIntent::Execute,
            Self::Query(_) => ExactSqlWriteIntent::Query,
            Self::ExecuteBatch(_) => ExactSqlWriteIntent::ExecuteBatch,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SqlResult {
    Validated,
    Executed(ExactSqlExecuteResult),
    Queried(ExactSqlRows),
    BatchExecuted(ExactSqlBatchResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactSqlWriteIntent {
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
pub(crate) enum TransactionPolicy {
    Ordinary,
    AuthorizedLongLease,
}

/// Whether one statement inside a transaction carries the ordinary
/// per-statement deadline.
///
/// `AuthorityRevalidated` is accepted only inside an
/// [`TransactionPolicy::AuthorizedLongLease`] transaction. It removes the
/// ordinary per-statement deadline while preserving shutdown cancellation and
/// repeated authority checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionPolicy {
    Bounded,
    AuthorityRevalidated,
}

pub trait ExactSqlWriteAuthority: Send + Sync {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExactSqlError {
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

impl fmt::Display for ExactSqlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityMismatch => {
                formatter.write_str("exact SQL authority does not match attached runtime")
            }
            Self::AuthorityDenied(reason) => {
                write!(formatter, "exact SQL write authority denied: {reason}")
            }
            Self::InvalidAttachment => formatter.write_str("exact SQL attachment is invalid"),
            Self::InvalidStatement => formatter.write_str("exact SQL statement is empty"),
            Self::RequestLimitExceeded => {
                formatter.write_str("exact SQL request exceeded its admission limit")
            }
            Self::TransactionControlDenied => {
                formatter.write_str("transaction control SQL is denied on the exact SQL channel")
            }
            Self::QueryLimitExceeded => {
                formatter.write_str("exact SQL query materialization exceeded its limit")
            }
            Self::Busy => formatter.write_str("exact SQL channel is busy"),
            Self::WriterUnavailable => formatter.write_str("exact SQL writer is unavailable"),
            Self::ReaderUnavailable(message) => {
                write!(formatter, "exact SQL reader is unavailable: {message}")
            }
            Self::TransactionClosed => {
                formatter.write_str("exact SQL transaction is already closed")
            }
            Self::TransactionExpired => formatter.write_str("exact SQL transaction lease expired"),
            Self::Sqlite {
                operation, message, ..
            } => {
                write!(formatter, "exact SQL {operation} failed: {message}")
            }
        }
    }
}

impl Error for ExactSqlError {}
