use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Engine(turso_core::LimboError),
    InvalidOperation(String),
    Denied(String),
    /// Transaction control or a savepoint outside what the capability allows.
    TransactionControlDenied,
    Cancelled,
    DeadlineExceeded,
    Authority(String),
    Busy,
    Unsupported(String),
    QueryLimitExceeded,
    RequestLimitExceeded,
    /// A statement ran past the connection's VM-operation budget.
    OperationLimitExceeded,
    /// The file is not a TraceDecay native store; the engine never saw it.
    ForeignFormat,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A constraint message is the schema's own text (a RAISE or the
            // violated CHECK), shown bare as sqlite3_errmsg shows it.
            Self::Engine(
                error @ (turso_core::LimboError::Constraint(_)
                | turso_core::LimboError::ForeignKeyConstraint(_)
                | turso_core::LimboError::Raise(..)),
            ) => write!(formatter, "{error}"),
            Self::Engine(error) => write!(formatter, "native Turso: {error}"),
            Self::InvalidOperation(message) => {
                write!(formatter, "invalid native operation: {message}")
            }
            Self::Denied(message) => write!(formatter, "native SQL denied: {message}"),
            Self::TransactionControlDenied => {
                formatter.write_str("transaction control is denied in this capability scope")
            }
            Self::Cancelled => formatter.write_str("native operation cancelled"),
            Self::DeadlineExceeded => formatter.write_str("native operation deadline exceeded"),
            Self::Authority(message) => write!(formatter, "native write authority: {message}"),
            Self::Busy => formatter.write_str("native database busy"),
            Self::QueryLimitExceeded => {
                formatter.write_str("native query materialization limit exceeded")
            }
            Self::RequestLimitExceeded => formatter.write_str("native SQL request limit exceeded"),
            Self::OperationLimitExceeded => {
                formatter.write_str("native statement exceeded its VM-operation budget")
            }
            Self::ForeignFormat => formatter
                .write_str("file is not a TraceDecay native store (header or application id)"),
            Self::Unsupported(message) => {
                write!(formatter, "unsupported native operation: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}
impl From<turso_core::LimboError> for Error {
    fn from(error: turso_core::LimboError) -> Self {
        match error {
            turso_core::LimboError::Busy => Self::Busy,
            error => Self::Engine(error),
        }
    }
}

impl Error {
    pub fn is_transient_read_failure(&self) -> bool {
        match self {
            Self::Busy | Self::Cancelled | Self::DeadlineExceeded => true,
            Self::Engine(error) => {
                if matches!(
                    error,
                    turso_core::LimboError::Busy
                        | turso_core::LimboError::BusySnapshot
                        | turso_core::LimboError::TableLocked
                        | turso_core::LimboError::Interrupt
                ) {
                    return true;
                }
                if let turso_core::LimboError::CompletionError(completion) = error {
                    if matches!(
                        completion,
                        turso_core::CompletionError::IOError(..)
                            | turso_core::CompletionError::Aborted
                            | turso_core::CompletionError::ShortWrite
                            | turso_core::CompletionError::ShortRead { .. }
                            | turso_core::CompletionError::ShortReadWalFrame { .. }
                    ) {
                        return true;
                    }
                    #[cfg(target_family = "unix")]
                    if matches!(completion, turso_core::CompletionError::RustixIOError(_)) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    pub const fn requires_transaction_retry(&self) -> bool {
        matches!(
            self,
            Self::Engine(
                turso_core::LimboError::BusySnapshot
                    | turso_core::LimboError::WriteWriteConflict
                    | turso_core::LimboError::CommitDependencyAborted
                    | turso_core::LimboError::SchemaConflict
            )
        )
    }

    pub const fn is_deterministic_refusal(&self) -> bool {
        matches!(
            self,
            Self::InvalidOperation(_)
                | Self::Denied(_)
                | Self::TransactionControlDenied
                | Self::Unsupported(_)
                | Self::RequestLimitExceeded
                | Self::OperationLimitExceeded
                | Self::ForeignFormat
                | Self::Engine(
                    turso_core::LimboError::Constraint(_)
                        | turso_core::LimboError::ForeignKeyConstraint(_)
                        | turso_core::LimboError::Raise(..)
                )
        )
    }
}
