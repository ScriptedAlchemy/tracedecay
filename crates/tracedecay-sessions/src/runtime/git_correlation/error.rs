/// Errors from the git-correlation store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCorrelationError {
    /// Underlying database failure.
    Db(String),
    /// Caller-supplied argument was invalid (bad ref kind, empty value, …).
    InvalidArgument(String),
}

impl std::fmt::Display for GitCorrelationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(message) => write!(f, "git correlation db error: {message}"),
            Self::InvalidArgument(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for GitCorrelationError {}

impl From<tracedecay_runtime_core::db::engine::Error> for GitCorrelationError {
    fn from(err: tracedecay_runtime_core::db::engine::Error) -> Self {
        Self::Db(err.to_string())
    }
}
