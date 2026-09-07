use std::error::Error;

use thiserror::Error;
use tracedecay_domain::errors::{AutomationErrorMessage, TraceDecayError};

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("config error: {message}")]
    Config { message: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("{port} port error: {source}")]
    Port {
        port: &'static str,
        #[source]
        source: BoxError,
    },
}

impl AutomationError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    pub fn port(port: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::Port {
            port,
            source: Box::new(source),
        }
    }
}

pub type Result<T> = std::result::Result<T, AutomationError>;

impl From<AutomationError> for TraceDecayError {
    fn from(error: AutomationError) -> Self {
        AutomationErrorMessage::new(error.to_string()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_error_boundary_preserves_type_message_and_classification() {
        let err = TraceDecayError::from(AutomationError::config("timed out waiting for backend"));

        assert!(matches!(&err, TraceDecayError::Automation(_)));
        assert_eq!(
            err.to_string(),
            "config error: timed out waiting for backend"
        );
        assert_eq!(
            crate::backend::classify_agent_task_error_message(&err.to_string()),
            crate::backend::AgentTaskFailureClass::Timeout,
        );
    }
}
