use std::fmt;

use tracedecay_sql::ErrorKind;

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
        kind: ErrorKind,
        message: String,
    },
    NativeTurso {
        message: String,
        deterministic: bool,
        transient_read: bool,
        transaction_retry: bool,
    },
    Cancelled,
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
            Self::NativeTurso { message, .. } => {
                write!(formatter, "native Turso failed: {message}")
            }
            Self::Cancelled => formatter.write_str("database operation cancelled"),
            Self::Busy => formatter.write_str("database runtime is busy"),
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
                kind,
                message,
            } => Self::Sqlite {
                operation,
                kind,
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

    /// The engine's classification of the failure, through a statement batch.
    pub const fn engine_kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Sqlite { kind, .. } => Some(*kind),
            Self::StatementBatch { source, .. } => source.engine_kind(),
            _ => None,
        }
    }

    /// True when replaying this exact statement can never succeed.
    ///
    /// A constraint abort is a schema-contract trigger or constraint refusing
    /// this exact row, and `InvalidOperation` is an admission or
    /// materialization ceiling refusing this exact statement. Neither is a
    /// transient engine condition, so a caller that retries one spins until
    /// something else changes the durable state.
    pub const fn is_deterministic_refusal(&self) -> bool {
        match self {
            Self::InvalidOperation(_)
            | Self::NativeTurso {
                deterministic: true,
                ..
            } => true,
            Self::StatementBatch { source, .. } => source.is_deterministic_refusal(),
            _ => matches!(self.engine_kind(), Some(ErrorKind::Constraint)),
        }
    }

    /// True when another connection holds the database or the runtime lane
    /// itself is saturated: the same statement can succeed once the holder
    /// releases.
    pub const fn is_busy_or_locked(&self) -> bool {
        match self {
            Self::Busy => true,
            Self::StatementBatch { source, .. } => source.is_busy_or_locked(),
            _ => matches!(self.engine_kind(), Some(ErrorKind::Busy)),
        }
    }

    pub const fn is_interrupted(&self) -> bool {
        matches!(self.engine_kind(), Some(ErrorKind::Interrupted))
    }

    /// True when a full-text index's shadow data is damaged.
    pub const fn is_corrupt_virtual_table(&self) -> bool {
        matches!(self.engine_kind(), Some(ErrorKind::CorruptVtab))
    }

    /// True when the whole native transaction must restart from a fresh snapshot.
    /// Retrying the failing statement in place would retain its conflicting state.
    pub const fn requires_transaction_retry(&self) -> bool {
        match self {
            Self::NativeTurso {
                transaction_retry, ..
            } => *transaction_retry,
            Self::StatementBatch { source, .. } => source.requires_transaction_retry(),
            _ => false,
        }
    }

    /// True when a held, interrupted or temporarily unavailable read can be tried again.
    pub const fn is_transient_read_failure(&self) -> bool {
        match self {
            Self::NativeTurso { transient_read, .. } => *transient_read,
            Self::TransactionExpired | Self::Cancelled => true,
            Self::StatementBatch { source, .. } => source.is_transient_read_failure(),
            _ => {
                self.is_busy_or_locked()
                    || matches!(
                        self.engine_kind(),
                        Some(ErrorKind::Interrupted | ErrorKind::IoErr | ErrorKind::CantOpen)
                    )
            }
        }
    }
}

impl From<tracedecay_sql::native::Error> for Error {
    fn from(error: tracedecay_sql::native::Error) -> Self {
        use tracedecay_sql::native::Error as Native;
        let deterministic = error.is_deterministic_refusal();
        let transient_read = error.is_transient_read_failure();
        let transaction_retry = error.requires_transaction_retry();
        match error {
            Native::InvalidOperation(message)
            | Native::Denied(message)
            | Native::Authority(message)
            | Native::Unsupported(message) => Self::InvalidOperation(message),
            Native::RequestLimitExceeded
            | Native::QueryLimitExceeded
            | Native::OperationLimitExceeded
            | Native::TransactionControlDenied
            | Native::ForeignFormat => Self::InvalidOperation(error.to_string()),
            Native::Busy => Self::Busy,
            Native::Cancelled => Self::Cancelled,
            Native::DeadlineExceeded => Self::TransactionExpired,
            Native::Engine(error) => Self::NativeTurso {
                message: error.to_string(),
                deterministic,
                transient_read,
                transaction_retry,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_sql::ErrorKind;

    use super::Error;

    fn engine(kind: ErrorKind) -> Error {
        Error::Sqlite {
            operation: "step",
            kind,
            message: "fixture".to_owned(),
        }
    }

    #[test]
    fn transient_read_failures_are_named_by_engine_kind() {
        for transient in [
            engine(ErrorKind::Busy),
            engine(ErrorKind::Interrupted),
            engine(ErrorKind::IoErr),
            engine(ErrorKind::CantOpen),
            Error::Busy,
        ] {
            assert!(transient.is_transient_read_failure(), "{transient:?}");
        }
        // A constraint abort, damaged pages, a column that does not decode:
        // each answers the same on every read.
        for verdict in [
            engine(ErrorKind::Constraint),
            engine(ErrorKind::Corrupt),
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
