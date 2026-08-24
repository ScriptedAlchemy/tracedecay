#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCorrelationError {
    Db(String),
    InvalidArgument(String),
    Contract(String),
    Corrupt(String),
    Unavailable(String),
    Cancelled,
    BudgetExhausted,
}

impl std::fmt::Display for GitCorrelationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(message) => write!(formatter, "git correlation receipt error: {message}"),
            Self::InvalidArgument(message) | Self::Contract(message) => {
                formatter.write_str(message)
            }
            Self::Corrupt(message) => {
                write!(formatter, "Git evidence projection is corrupt: {message}")
            }
            Self::Unavailable(message) => {
                write!(
                    formatter,
                    "Git evidence projection is unavailable: {message}"
                )
            }
            Self::Cancelled => formatter.write_str("Git evidence operation was cancelled"),
            Self::BudgetExhausted => {
                formatter.write_str("Git evidence operation exhausted its budget")
            }
        }
    }
}

impl std::error::Error for GitCorrelationError {}

impl From<tracedecay_runtime_core::db::engine::Error> for GitCorrelationError {
    fn from(error: tracedecay_runtime_core::db::engine::Error) -> Self {
        Self::Db(error.to_string())
    }
}

impl From<serde_json::Error> for GitCorrelationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Corrupt(error.to_string())
    }
}

impl From<tracedecay_graph_db::GraphDbError> for GitCorrelationError {
    fn from(error: tracedecay_graph_db::GraphDbError) -> Self {
        use tracedecay_graph_db::GraphDbError;

        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::BudgetExhausted { .. } | GraphDbError::DeadlineExceeded => {
                Self::BudgetExhausted
            }
            GraphDbError::InvalidRequest { message } => Self::Contract(message),
            GraphDbError::Corrupt { message }
            | GraphDbError::ResetRequired { message }
            | GraphDbError::DurabilityUncertain { message }
            | GraphDbError::ProjectionMismatch { message, .. }
            | GraphDbError::GenerationMismatch { message, .. } => Self::Corrupt(message),
            GraphDbError::Conflict => {
                Self::Unavailable("Git evidence publication conflict".to_owned())
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("graph store is closed".to_owned()),
        }
    }
}
