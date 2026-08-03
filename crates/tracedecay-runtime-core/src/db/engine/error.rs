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
            Self::TransactionClosed => formatter.write_str("SQLite transaction is closed"),
            Self::TransactionExpired => formatter.write_str("SQLite transaction lease expired"),
        }
    }
}

impl std::error::Error for Error {}

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

    pub const fn sqlite_code(&self) -> Option<i32> {
        match self {
            Self::Sqlite { code, .. } => *code,
            _ => None,
        }
    }

    pub const fn sqlite_extended_code(&self) -> Option<i32> {
        match self {
            Self::Sqlite { extended_code, .. } => *extended_code,
            _ => None,
        }
    }
}
