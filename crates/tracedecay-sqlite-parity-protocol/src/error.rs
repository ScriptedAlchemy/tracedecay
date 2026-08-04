use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RequestTooLarge,
    InvalidRequest,
    UnsupportedProtocolVersion,
    InvalidPath,
    InvalidSnapshotProvenance,
    RefusedLiveProfile,
    OpenFailed,
    ReadOnlyInvariant,
    InvalidStoreFamily,
    InvalidPageCursor,
    InvalidPageLimit,
    ResultLimitExceeded,
    InvalidSqliteValue,
    InvalidSqliteHeader,
    SqliteFailure,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite_code: Option<String>,
}

impl ErrorPayload {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            sqlite_code: None,
        }
    }

    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}
