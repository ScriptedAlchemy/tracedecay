#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCorrelationError {
    Db(String),
    InvalidArgument(String),
    Contract(String),
    Corrupt(String),
    Unavailable(String),
}

impl std::fmt::Display for GitCorrelationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(message) => write!(formatter, "git correlation receipt error: {message}"),
            Self::InvalidArgument(message) | Self::Contract(message) => {
                formatter.write_str(message)
            }
            Self::Corrupt(message) => {
                write!(formatter, "Git evidence rows are corrupt: {message}")
            }
            Self::Unavailable(message) => {
                write!(formatter, "Git evidence is unavailable: {message}")
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
