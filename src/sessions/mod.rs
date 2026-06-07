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

/// Search hit for session-message full-text lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}
