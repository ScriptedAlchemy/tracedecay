/// Operational analyzer-process failure retained for daemon-local reporting.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnalyzerRuntimeError {
    #[error("analyzer unavailable")]
    Unavailable,
    #[error("config error: {message}")]
    Config { message: String },
}

impl AnalyzerRuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable => "analyzer unavailable",
            Self::Config { message } => message,
        }
    }
}

impl From<std::io::Error> for AnalyzerRuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for AnalyzerRuntimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub type AnalyzerResult<T> = Result<T, AnalyzerRuntimeError>;

/// Cloneable cancellation signal for one analyzer operation.
pub type AnalyzerCancellation = tokio_util::sync::CancellationToken;
