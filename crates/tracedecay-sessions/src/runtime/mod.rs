use serde::{Deserialize, Serialize};

pub use tracedecay_store::{SessionMessageRecord, SessionRecord};

// Runtime modules are public because the root composition crate mounts these
// concrete provider and storage authorities directly.
mod hosts;
pub use hosts::{
    claude, claude_observation, cline_like, codex, codex_app_server, cursor, cursor_composer,
    hermes, kimi, kiro, opencode, vibe,
};
pub(in crate::runtime) use hosts::{opencode_frontier, opencode_part_scan, opencode_snapshot};
pub mod git_correlation;
mod host_scan;
pub mod ingest;
mod native_ingest_source;
pub use native_ingest_source::native_ingest_source_identity;
mod observation;
pub use observation::snapshot_observation;
pub(in crate::runtime) use observation::{ingest_byte_budget, jsonl_observation_admission};
mod pipeline_metrics;
pub mod registered_db;
pub mod shared;
pub mod source;
pub mod store_access;
pub mod store_port;
mod workflow;
pub use workflow::{workflow_index, workflow_ingest, workflow_state};

pub use crate::{ProviderScope, SessionProvider};
// Shared full-text/LCM retrieval filters are owned by the LCM engine crate;
// the session search surface re-imports them so both sides filter identically.
pub use ingest::{
    IngestPassCoverage, TranscriptCatchUpFailure, TranscriptIngestDisposition,
    TranscriptIngestOutcome, classify_claude_observation_failure,
    classify_transcript_ingest_disposition, classify_transcript_ingest_failure, home_dir,
    ingest_project_sources_for_provider, ingest_project_sources_for_provider_with_cancellation,
    ingest_project_sources_for_provider_with_cancellation_and_codex_state,
    ingest_user_global_sources_for_provider_with_authorities,
    ingest_user_global_sources_for_provider_with_authorities_and_cancellation,
    ingest_user_global_sources_for_startup_with_db,
    ingest_user_global_sources_for_startup_with_db_and_codex_state, registered_project_roots_from,
    try_ingest_user_codex_sessions_with_db_and_admission, with_transcript_source_home,
};
pub use ingest::{USER_SESSIONS_DB_FILENAME, user_sessions_db_path};
pub use registered_db::{
    SessionExec, SessionQuery, SessionRegisteredDb, SessionStoreAccess, SessionWriteTxn,
};
pub use shared::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
/// Public because the snapshot capture entry points that return it are public.
pub use snapshot_observation::SnapshotCaptureOutcome;
pub use store_access::{
    SessionActivityRow, SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    TranscriptBatch, TranscriptGitEvidence, TranscriptPersistenceError,
};
pub use tracedecay_lcm::{SessionMessageType, SessionSearchScope};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageSearchResult {
    pub session: SessionRecord,
    pub message: SessionMessageRecord,
    pub score: f64,
}

/// Inclusive timestamp bounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchTimeRange {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

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
