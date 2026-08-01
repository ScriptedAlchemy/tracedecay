use serde::{Deserialize, Serialize};

pub use tracedecay_store::{SessionMessageRecord, SessionRecord};

// Everything the root crate could once reach through `crate::sessions::…` is
// `pub` here: crate-private visibility would now stop at the tracedecay-sessions
// boundary and silently drop that surface from the root shim.
pub mod claude;
pub mod claude_observation;
pub mod cline_like;
pub mod codex;
pub mod codex_app_server;
pub mod cursor;
pub mod cursor_agent;
pub mod cursor_composer;
pub mod git_correlation;
pub mod hermes;
pub mod ingest;
mod ingest_byte_budget;
mod jsonl_observation_admission;
pub mod kiro;
pub mod lcm;
pub mod shared;
pub mod snapshot_observation;
pub mod source;
pub mod store_port;
// Exposes three `#[doc(hidden)]` process-safety test helpers that the root
// integration tests reach through the `crate::sessions` shim.
pub mod transcript_backfill;
pub mod vibe;
pub mod workflow_index;
pub mod workflow_ingest;
pub mod workflow_state;

pub use crate::{ProviderScope, SessionProvider};
pub use ingest::{
    TranscriptCatchUpFailure, classify_claude_observation_failure,
    classify_transcript_ingest_failure, home_dir, ingest_project_sources_for_provider,
    ingest_project_sources_for_provider_with_cancellation,
    ingest_user_global_sources_for_provider_with_authorities,
    ingest_user_global_sources_for_startup_with_db, registered_project_roots_from,
    try_ingest_user_codex_sessions_with_db_and_admission, with_transcript_source_home,
};
pub use ingest::{USER_SESSIONS_DB_FILENAME, user_sessions_db_path};
pub use shared::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
/// Public because the snapshot capture entry points that return it are public.
pub use snapshot_observation::SnapshotCaptureOutcome;

/// Search hit for session-message full-text lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}

/// Inclusive timestamp bounds for session-message full-text search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchTimeRange {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

/// Relationship and time filters for session-message full-text search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSearchFilters<'a> {
    pub scope: SessionSearchScope,
    pub message_type: SessionMessageType,
    pub parent_session_id: Option<&'a str>,
    pub time_range: SessionSearchTimeRange,
}

impl Default for SessionSearchFilters<'_> {
    fn default() -> Self {
        Self {
            scope: SessionSearchScope::All,
            message_type: SessionMessageType::All,
            parent_session_id: None,
            time_range: SessionSearchTimeRange::default(),
        }
    }
}

/// Scope filter for session-message full-text search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSearchScope {
    All,
    ParentsOnly,
    SubagentsOnly,
}

impl SessionSearchScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "parents_only" => Some(Self::ParentsOnly),
            "subagents_only" => Some(Self::SubagentsOnly),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ParentsOnly => "parents_only",
            Self::SubagentsOnly => "subagents_only",
        }
    }
}

/// Semantic message filter shared by full-text and LCM retrieval. Providers
/// sometimes encode tool results with role `user`, so this is intentionally
/// stronger than the raw role filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMessageType {
    #[default]
    All,
    DirectUser,
    ToolResult,
}

impl SessionMessageType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }
}
