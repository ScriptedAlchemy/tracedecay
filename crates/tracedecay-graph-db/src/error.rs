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

pub(crate) fn rollback_failure(
    context: &str,
    primary: impl std::fmt::Display,
    rollback: impl std::fmt::Display,
) -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: format!("{context} failure `{primary}` followed by rollback failure: {rollback}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphDbError, rollback_failure};

    #[test]
    fn rollback_failure_preserves_both_errors_and_context() {
        assert_eq!(
            rollback_failure("format initialization", "create failed", "rollback failed"),
            GraphDbError::DurabilityUncertain {
                message: "format initialization failure `create failed` followed by rollback failure: rollback failed"
                    .to_owned(),
            }
        );
    }
}
