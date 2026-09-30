use std::error::Error;
use std::future::Future;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Provider-neutral metadata for an indexed agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub provider: String,
    pub session_id: String,
    pub project_key: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub metadata_json: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_subagent: bool,
    pub agent_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
}

/// Provider-neutral message payload extracted from an agent transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessageRecord {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub role: String,
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub text: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<String>,
    pub source_path: Option<String>,
    pub source_offset: Option<i64>,
    pub metadata_json: Option<String>,
}

/// Persisted parse cursor for one transcript path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ParseOffset {
    pub byte_offset: u64,
    pub mtime: u64,
    pub file_id: u64,
}

/// Validated cursor advance for parsed transcript input that emitted no
/// messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptWriteBatch {
    cursor_path: PathBuf,
    expected_offset: ParseOffset,
    next_offset: ParseOffset,
}

impl TranscriptWriteBatch {
    /// Builds an offset-only write for parsed input that emitted no messages.
    pub fn advance_offset(
        cursor_path: PathBuf,
        expected_offset: ParseOffset,
        next_offset: ParseOffset,
    ) -> TranscriptStoreResult<Self> {
        if cursor_path.as_os_str().is_empty() {
            return Err(TranscriptStoreError::InvalidCursorPath);
        }

        Ok(Self {
            cursor_path,
            expected_offset,
            next_offset,
        })
    }

    /// Consumes this validated request into its cursor path, the durable
    /// cursor the writer observed before parsing, and the cursor to persist.
    pub fn into_parts(self) -> (PathBuf, ParseOffset, ParseOffset) {
        (self.cursor_path, self.expected_offset, self.next_offset)
    }
}

/// Explicit failure returned by the authoritative transcript store.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptStoreError {
    #[error("transcript cursor path must not be empty")]
    InvalidCursorPath,
    #[error(
        "transcript cursor conflict for {cursor_path:?}: expected {expected:?}, found {actual:?}"
    )]
    Conflict {
        cursor_path: PathBuf,
        expected: ParseOffset,
        actual: ParseOffset,
    },
    #[error("transcript storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type TranscriptStoreResult<T> = Result<T, TranscriptStoreError>;

/// Narrow store-facing boundary for restart-safe transcript persistence.
///
/// Implementations load the authoritative durable offset and persist exactly one
/// write. Git correlation and other application projections remain outside this
/// contract. No fallback destination is permitted on error.
pub trait TranscriptStore: Send + Sync {
    /// Loads the durable cursor, returning the default cursor when untracked.
    fn get_parse_offset(
        &self,
        cursor_path: &Path,
    ) -> impl Future<Output = TranscriptStoreResult<ParseOffset>> + Send;

    /// Persists one cursor advance in the authoritative store.
    fn persist_transcript_batch(
        &self,
        batch: TranscriptWriteBatch,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_offset_rejects_an_empty_cursor_path() {
        let batch = TranscriptWriteBatch::advance_offset(
            PathBuf::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::InvalidCursorPath)
        ));
    }
}
