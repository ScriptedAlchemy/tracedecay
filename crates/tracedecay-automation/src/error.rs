use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationError {
    message: String,
}

impl AutomationError {
    pub fn config(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AutomationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "config error: {}", self.message)
    }
}

impl std::error::Error for AutomationError {}

pub type Result<T> = std::result::Result<T, AutomationError>;

pub(crate) fn config_error(message: impl Into<String>) -> AutomationError {
    AutomationError::config(message)
}
