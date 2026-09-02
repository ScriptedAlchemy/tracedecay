use std::error::Error;

use tracedecay_store::{ParseOffset, SessionMessageRecord, SessionRecord};

/// Inclusive unix-millis threshold used to normalize mixed-resolution timestamps.
pub const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

/// One ingested session message, projected to the fields the hint-outcome
/// correlator needs: the timestamp/ordinal that order activity after a hint and
/// the tool-activity carriers (`kind='tool_event'` + `tool_names` for Codex,
/// `tool_names`/`metadata_json.tool_events` for Claude/Cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionActivityRow {
    pub timestamp: Option<i64>,
    pub ordinal: i64,
    pub kind: Option<String>,
    pub tool_names: Option<String>,
    pub metadata_json: Option<String>,
}

/// Transcript-ingest backlog snapshot for a session store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionProviderCoverageState {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionProviderCoverage {
    pub provider: String,
    pub state: SessionProviderCoverageState,
    pub deferred_units: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionIngestHealth {
    /// Providers with durable session rows or daemon-owned source frontiers.
    pub observed_providers: Vec<String>,
    /// Latest daemon-owned completion state for each bounded provider sweep.
    pub provider_coverage: Vec<SessionProviderCoverage>,
    /// Transcripts referenced by sessions that still exist on disk.
    pub tracked_transcripts: u64,
    /// Transcripts with un-ingested appended bytes.
    pub pending_transcripts: u64,
    /// Total un-ingested bytes across pending transcripts.
    pub pending_bytes: u64,
    /// Largest single-transcript backlog. The hook ingest caps are
    /// per-transcript, so this (not the total) decides whether the hooks can
    /// still drain the backlog on their own.
    pub max_transcript_pending_bytes: u64,
    /// Newest transcript mtime recorded at ingest time (Unix seconds).
    pub last_ingest_unix: Option<i64>,
}

/// One transcript session plus its parsed messages, for projection-only
/// multi-session upserts from stores such as Hermes `state.db`.
///
/// This compatibility DTO remains local because projection-only persistence is
/// intentionally outside the authoritative transcript store contract.
#[derive(Debug, Clone)]
pub struct TranscriptBatch {
    pub session: SessionRecord,
    pub messages: Vec<SessionMessageRecord>,
}

#[derive(Debug)]
pub enum TranscriptPersistenceError {
    Conflict {
        expected: ParseOffset,
        actual: ParseOffset,
    },
    PairConflict {
        path: String,
        expected: ParseOffset,
        actual: ParseOffset,
    },
    Storage {
        operation: &'static str,
        source: Box<dyn Error + Send + Sync>,
    },
}

impl TranscriptPersistenceError {
    pub fn storage(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::Storage {
            operation,
            source: Box::new(source),
        }
    }

    pub fn message(operation: &'static str, message: impl Into<String>) -> Self {
        Self::storage(operation, std::io::Error::other(message.into()))
    }
}

impl std::fmt::Display for TranscriptPersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                formatter,
                "transcript parse offset conflict: expected {expected:?}, actual {actual:?}"
            ),
            Self::PairConflict {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "transcript parse offset pair conflict at {path}: expected {expected:?}, actual {actual:?}"
            ),
            Self::Storage { operation, source } => write!(formatter, "{operation}: {source}"),
        }
    }
}

impl Error for TranscriptPersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conflict { .. } | Self::PairConflict { .. } => None,
            Self::Storage { source, .. } => Some(source.as_ref()),
        }
    }
}
