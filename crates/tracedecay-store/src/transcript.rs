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

/// Validated authoritative transcript persistence request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptWriteBatch {
    cursor_path: PathBuf,
    kind: TranscriptWriteKind,
}

/// Consumed representation of a validated transcript write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptWriteKind {
    /// Advances the cursor after parsing input that emitted no messages.
    AdvanceOffset {
        expected_offset: ParseOffset,
        next_offset: ParseOffset,
    },
    /// Atomically persists a session, its messages, and the next cursor.
    Upsert {
        session: Box<SessionRecord>,
        messages: Vec<SessionMessageRecord>,
        expected_offset: ParseOffset,
        next_offset: ParseOffset,
    },
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
            kind: TranscriptWriteKind::AdvanceOffset {
                expected_offset,
                next_offset,
            },
        })
    }

    /// Builds a full atomic session/message/offset write.
    pub fn upsert(
        session: SessionRecord,
        messages: Vec<SessionMessageRecord>,
        expected_offset: ParseOffset,
        next_offset: ParseOffset,
    ) -> TranscriptStoreResult<Self> {
        let cursor_path = session
            .transcript_path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| TranscriptStoreError::MissingTranscriptPath {
                provider: session.provider.clone(),
                session_id: session.session_id.clone(),
            })?;
        Self::upsert_with_cursor(cursor_path, session, messages, expected_offset, next_offset)
    }

    /// Builds a full atomic write whose durable cursor key differs from the
    /// session's physical transcript path.
    ///
    /// Virtual transcript sources use a stable logical cursor while retaining
    /// the real source path in [`SessionRecord::transcript_path`].
    pub fn upsert_with_cursor(
        cursor_path: PathBuf,
        session: SessionRecord,
        messages: Vec<SessionMessageRecord>,
        expected_offset: ParseOffset,
        next_offset: ParseOffset,
    ) -> TranscriptStoreResult<Self> {
        let session_path = session.transcript_path.as_deref().ok_or_else(|| {
            TranscriptStoreError::MissingTranscriptPath {
                provider: session.provider.clone(),
                session_id: session.session_id.clone(),
            }
        })?;
        if session_path.is_empty() {
            return Err(TranscriptStoreError::InvalidTranscriptPath);
        }
        if cursor_path.as_os_str().is_empty() {
            return Err(TranscriptStoreError::InvalidCursorPath);
        }

        if let Some(message) = messages.iter().find(|message| {
            message.provider != session.provider || message.session_id != session.session_id
        }) {
            return Err(TranscriptStoreError::MessageIdentityMismatch {
                message_id: message.message_id.clone(),
                expected_provider: session.provider,
                actual_provider: message.provider.clone(),
                expected_session_id: session.session_id,
                actual_session_id: message.session_id.clone(),
            });
        }

        Ok(Self {
            cursor_path,
            kind: TranscriptWriteKind::Upsert {
                session: Box::new(session),
                messages,
                expected_offset,
                next_offset,
            },
        })
    }

    /// Returns the durable cursor identity represented by this write.
    pub fn cursor_path(&self) -> &Path {
        &self.cursor_path
    }

    /// Returns the durable cursor that the writer observed before parsing.
    pub fn expected_offset(&self) -> ParseOffset {
        match &self.kind {
            TranscriptWriteKind::AdvanceOffset {
                expected_offset, ..
            }
            | TranscriptWriteKind::Upsert {
                expected_offset, ..
            } => *expected_offset,
        }
    }

    /// Consumes this validated request for persistence.
    pub fn into_parts(self) -> (PathBuf, TranscriptWriteKind) {
        (self.cursor_path, self.kind)
    }
}

/// Explicit failure returned by the authoritative transcript store.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptStoreError {
    #[error("transcript cursor path must not be empty")]
    InvalidCursorPath,
    #[error("transcript path must not be empty")]
    InvalidTranscriptPath,
    #[error("session {provider}/{session_id} has no transcript path")]
    MissingTranscriptPath {
        provider: String,
        session_id: String,
    },
    #[error(
        "message {message_id} identity {actual_provider}/{actual_session_id} does not match session {expected_provider}/{expected_session_id}"
    )]
    MessageIdentityMismatch {
        message_id: String,
        expected_provider: String,
        actual_provider: String,
        expected_session_id: String,
        actual_session_id: String,
    },
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

    /// Persists one offset-only or full atomic write in the authoritative store.
    fn persist_transcript_batch(
        &self,
        batch: TranscriptWriteBatch,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(transcript_path: Option<&str>) -> SessionRecord {
        SessionRecord {
            provider: "test".into(),
            session_id: "session".into(),
            project_key: "project".into(),
            project_path: "/project".into(),
            title: None,
            started_at: None,
            ended_at: None,
            transcript_path: transcript_path.map(str::to_owned),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    fn message(provider: &str, session_id: &str) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: provider.into(),
            message_id: "message".into(),
            session_id: session_id.into(),
            role: "user".into(),
            timestamp: None,
            ordinal: 0,
            text: "hello".into(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }
    }

    #[test]
    fn advance_offset_is_an_explicit_valid_batch() {
        let batch = TranscriptWriteBatch::advance_offset(
            PathBuf::from("session.jsonl"),
            ParseOffset::default(),
            ParseOffset {
                byte_offset: 42,
                mtime: 7,
                file_id: 9,
            },
        )
        .unwrap();

        assert_eq!(batch.cursor_path(), Path::new("session.jsonl"));
    }

    #[test]
    fn upsert_uses_the_session_transcript_path() {
        let batch = TranscriptWriteBatch::upsert(
            session(Some("session.jsonl")),
            Vec::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        )
        .unwrap();

        assert_eq!(batch.cursor_path(), Path::new("session.jsonl"));
    }

    #[test]
    fn upsert_with_cursor_preserves_the_physical_session_path() {
        let batch = TranscriptWriteBatch::upsert_with_cursor(
            PathBuf::from("cursor-chat:agent-1"),
            session(Some("/physical/store.db")),
            Vec::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        )
        .unwrap();

        assert_eq!(batch.cursor_path(), Path::new("cursor-chat:agent-1"));
        let (_, kind) = batch.into_parts();
        assert!(matches!(
            kind,
            TranscriptWriteKind::Upsert { session, .. }
                if session.transcript_path.as_deref() == Some("/physical/store.db")
        ));
    }

    #[test]
    fn upsert_with_cursor_rejects_an_empty_cursor_path() {
        let batch = TranscriptWriteBatch::upsert_with_cursor(
            PathBuf::new(),
            session(Some("/physical/store.db")),
            Vec::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::InvalidCursorPath)
        ));
    }

    #[test]
    fn upsert_requires_a_session_transcript_path() {
        let batch = TranscriptWriteBatch::upsert(
            session(None),
            Vec::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::MissingTranscriptPath { .. })
        ));
    }

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

    #[test]
    fn upsert_rejects_an_empty_transcript_path() {
        let batch = TranscriptWriteBatch::upsert(
            session(Some("")),
            Vec::new(),
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::InvalidTranscriptPath)
        ));
    }

    #[test]
    fn upsert_rejects_a_foreign_message_provider() {
        let batch = TranscriptWriteBatch::upsert(
            session(Some("session.jsonl")),
            vec![message("other", "session")],
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::MessageIdentityMismatch { .. })
        ));
    }

    #[test]
    fn upsert_rejects_a_foreign_message_session() {
        let batch = TranscriptWriteBatch::upsert(
            session(Some("session.jsonl")),
            vec![message("test", "other")],
            ParseOffset::default(),
            ParseOffset::default(),
        );

        assert!(matches!(
            batch,
            Err(TranscriptStoreError::MessageIdentityMismatch { .. })
        ));
    }

    #[test]
    fn consumed_write_preserves_expected_cursor() {
        let expected_offset = ParseOffset {
            byte_offset: 12,
            mtime: 3,
            file_id: 4,
        };
        let batch = TranscriptWriteBatch::advance_offset(
            PathBuf::from("session.jsonl"),
            expected_offset,
            ParseOffset::default(),
        )
        .unwrap();

        assert_eq!(batch.expected_offset(), expected_offset);
        let (cursor_path, kind) = batch.into_parts();
        assert_eq!(cursor_path, PathBuf::from("session.jsonl"));
        assert!(matches!(
            kind,
            TranscriptWriteKind::AdvanceOffset {
                expected_offset: actual,
                ..
            } if actual == expected_offset
        ));
    }
}
