use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphDbError {
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid graph database request: {message}")]
    InvalidRequest { message: String },
    #[error("graph database conflict")]
    Conflict,
    #[error("graph operation budget exhausted")]
    BudgetExhausted,
    #[error("graph database reset required: {message}")]
    ResetRequired { message: String },
    #[error("graph database is corrupt: {message}")]
    Corrupt { message: String },
    #[error("graph database unavailable: {message}")]
    Unavailable { message: String },
    #[error("graph database durability is uncertain: {message}")]
    DurabilityUncertain { message: String },
    #[error("graph database is closed")]
    Closed,
}

impl GraphDbError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable {
            message: message.into(),
        }
    }
}
