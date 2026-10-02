use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidColumn(i32),
    TypeMismatch {
        column: i32,
        expected: &'static str,
        actual: &'static str,
    },
    IntegerOutOfRange {
        column: i32,
        target: &'static str,
        value: i64,
    },
    Runtime(String),
    Sqlite {
        operation: &'static str,
        code: Option<i32>,
        extended_code: Option<i32>,
        message: String,
    },
    Busy,
    InvalidOperation(String),
    StatementBatch {
        index: usize,
        source: Box<Error>,
    },
    TransactionClosed,
    TransactionExpired,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidColumn(column) => write!(formatter, "invalid column index {column}"),
            Self::TypeMismatch {
                column,
                expected,
                actual,
            } => write!(
                formatter,
                "column {column} has SQLite type {actual}; expected {expected}"
            ),
            Self::IntegerOutOfRange {
                column,
                target,
                value,
            } => write!(
                formatter,
                "column {column} integer {value} is out of range for {target}"
            ),
            Self::Runtime(message) => write!(formatter, "SQLite runtime failed: {message}"),
            Self::Sqlite {
                operation, message, ..
            } => write!(formatter, "SQLite {operation} failed: {message}"),
            Self::Busy => formatter.write_str("SQLite runtime is busy"),
            Self::InvalidOperation(message) => formatter.write_str(message),
            Self::StatementBatch { index, source } => {
                write!(
                    formatter,
                    "SQLite statement batch failed at index {index}: {source}"
                )
            }
            Self::TransactionClosed => formatter.write_str("SQLite transaction is closed"),
            Self::TransactionExpired => formatter.write_str("SQLite transaction lease expired"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StatementBatch { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<tracedecay_rusqlite_runtime::exact_sql::ExactSqlError> for Error {
    fn from(error: tracedecay_rusqlite_runtime::exact_sql::ExactSqlError) -> Self {
        use tracedecay_rusqlite_runtime::exact_sql::ExactSqlError;

        match error {
            ExactSqlError::InvalidStatement => {
                Self::InvalidOperation("SQL statement is empty".to_owned())
            }
            ExactSqlError::TransactionControlDenied => Self::InvalidOperation(
                "transaction control SQL is not allowed inside an owned transaction".to_owned(),
            ),
            ExactSqlError::TransactionClosed => Self::TransactionClosed,
            ExactSqlError::TransactionExpired => Self::TransactionExpired,
            ExactSqlError::RequestLimitExceeded => {
                Self::InvalidOperation("SQL request exceeds migration limits".to_owned())
            }
            // A materialization ceiling is a property of the submitted
            // statement, not of the engine: the untyped `Runtime` fallback
            // made callers read it as a transient storage fault and replay it.
            ExactSqlError::QueryLimitExceeded => Self::InvalidOperation(
                "exact SQL query materialization exceeded its limit".to_owned(),
            ),
            ExactSqlError::AuthorityDenied(message) => Self::InvalidOperation(message),
            ExactSqlError::Sqlite {
                operation,
                code,
                extended_code,
                message,
            } => Self::Sqlite {
                operation,
                code,
                extended_code,
                message,
            },
            ExactSqlError::Busy => Self::Busy,
            error => Self::Runtime(error.to_string()),
        }
    }
}

impl Error {
    pub fn invalid_operation(message: impl Into<String>) -> Self {
        Self::InvalidOperation(message.into())
    }

    pub(crate) fn statement_batch(index: usize, source: Self) -> Self {
        Self::StatementBatch {
            index,
            source: Box::new(source),
        }
    }

    #[hotpath::skip]
    pub const fn sqlite_code(&self) -> Option<i32> {
        match self {
            Self::Sqlite { code, .. } => *code,
            Self::StatementBatch { source, .. } => source.sqlite_code(),
            _ => None,
        }
    }

    #[hotpath::skip]
    pub const fn sqlite_extended_code(&self) -> Option<i32> {
        match self {
            Self::Sqlite { extended_code, .. } => *extended_code,
            Self::StatementBatch { source, .. } => source.sqlite_extended_code(),
            _ => None,
        }
    }

    /// True when replaying this exact statement can never succeed.
    ///
    /// A `SQLITE_CONSTRAINT` abort is a schema-contract trigger or constraint
    /// refusing this exact row, and `InvalidOperation` is an admission or
    /// materialization ceiling refusing this exact statement. Neither is a
    /// transient engine condition, so a caller that retries one spins until
    /// something else changes the durable state.
    #[hotpath::skip]
    pub const fn is_deterministic_refusal(&self) -> bool {
        match self {
            Self::InvalidOperation(_) => true,
            Self::StatementBatch { source, .. } => source.is_deterministic_refusal(),
            _ => matches!(self.sqlite_code(), Some(SQLITE_CONSTRAINT)),
        }
    }

    /// True when another connection holds the database (`SQLITE_BUSY` or
    /// `SQLITE_LOCKED`) or the runtime lane itself is saturated: the same
    /// statement can succeed once the holder releases.
    #[hotpath::skip]
    pub const fn is_busy_or_locked(&self) -> bool {
        match self {
            Self::Busy => true,
            Self::StatementBatch { source, .. } => source.is_busy_or_locked(),
            _ => matches!(self.sqlite_code(), Some(SQLITE_BUSY | SQLITE_LOCKED)),
        }
    }

    /// True when this failure says the database could not be read right now
    /// (held, interrupted, an I/O fault, or a file that would not open), so
    /// the same read can succeed later without anything changing.
    #[hotpath::skip]
    pub const fn is_transient_read_failure(&self) -> bool {
        match self {
            Self::TransactionExpired => true,
            Self::StatementBatch { source, .. } => source.is_transient_read_failure(),
            _ => {
                self.is_busy_or_locked()
                    || matches!(
                        self.sqlite_code(),
                        Some(SQLITE_INTERRUPT | SQLITE_IOERR | SQLITE_CANTOPEN)
                    )
            }
        }
    }
}

/// `SQLITE_BUSY`: another connection holds a conflicting database lock.
const SQLITE_BUSY: i32 = 5;
/// `SQLITE_LOCKED`: a conflicting lock inside the same shared cache.
const SQLITE_LOCKED: i32 = 6;
/// `SQLITE_CONSTRAINT`: a constraint or `RAISE(ABORT)` trigger refused the row.
const SQLITE_CONSTRAINT: i32 = 19;
const SQLITE_INTERRUPT: i32 = 9;
const SQLITE_IOERR: i32 = 10;
const SQLITE_CANTOPEN: i32 = 14;

#[cfg(test)]
mod tests {
    use super::Error;

    fn sqlite(extended_code: i32) -> Error {
        Error::Sqlite {
            operation: "step",
            code: Some(extended_code & 0xff),
            extended_code: Some(extended_code),
            message: "fixture".to_owned(),
        }
    }

    #[test]
    fn transient_read_failures_are_named_by_engine_code() {
        // SQLITE_BUSY_SNAPSHOT, SQLITE_LOCKED, SQLITE_IOERR_SHORT_READ.
        for transient in [sqlite(517), sqlite(6), sqlite(522), Error::Busy] {
            assert!(transient.is_transient_read_failure(), "{transient:?}");
        }
        // SQLITE_CONSTRAINT_TRIGGER, SQLITE_CORRUPT, a column that does not
        // decode: each answers the same on every read.
        for verdict in [
            sqlite(1811),
            sqlite(11),
            Error::TypeMismatch {
                column: 0,
                expected: "text",
                actual: "integer",
            },
        ] {
            assert!(!verdict.is_transient_read_failure(), "{verdict:?}");
        }
    }
}
