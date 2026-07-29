use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

/// Operational analyzer-process failure retained for daemon-local reporting.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AnalyzerRuntimeError {
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

#[derive(Default)]
struct AnalyzerCancellationInner {
    cancelled: AtomicBool,
    notification: Notify,
}

/// Cloneable cancellation signal for one analyzer operation.
#[derive(Clone, Default)]
pub struct AnalyzerCancellation {
    inner: Arc<AnalyzerCancellationInner>,
}

impl AnalyzerCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notification.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notification.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}
