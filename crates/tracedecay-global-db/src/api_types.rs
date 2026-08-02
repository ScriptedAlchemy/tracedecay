use tracedecay_sessions::runtime::lcm::LcmSummaryRequest;
use tracedecay_store::{SessionMessageRecord, SessionRecord};

/// Total savings + call count for a project (or all projects when `project` is None).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsTotal {
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SavingsDay {
    /// Start-of-day epoch seconds (UTC).
    pub day: i64,
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventInsert {
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsEventRecord {
    pub id: i64,
    pub provider: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub timestamp: i64,
    pub event_kind: String,
    pub hook_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_category: Option<String>,
    pub skill_name: Option<String>,
    pub hint_category: Option<String>,
    pub hint_id: Option<String>,
    pub outcome: Option<String>,
    pub metadata_json: Option<String>,
}

impl tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestampSource
    for AnalyticsEventRecord
{
    fn as_analytics_session_timestamp(
        &self,
    ) -> Option<tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestamp> {
        Some(
            tracedecay_sessions::runtime::git_correlation::AnalyticsSessionTimestamp {
                provider: self.provider.clone(),
                session_id: self.session_id.clone()?,
                timestamp: self.timestamp,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsToolCounts {
    pub tool_name: String,
    pub calls: i64,
    pub errors: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsHintCounts {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnalyticsEventQuery {
    pub provider: Option<String>,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub event_kind: Option<String>,
    /// Inclusive lower bound on `timestamp` (unix seconds). `None` = unbounded.
    pub since: Option<i64>,
    /// Exclusive upper bound on `timestamp` (unix seconds). `None` = unbounded.
    pub until: Option<i64>,
    /// Exclusive row-id cursor used by bounded reverse-chronological scans.
    pub before_id: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingCodexCompactionSummary {
    pub node_id: String,
    pub request: LcmSummaryRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeProjectRecord {
    pub project_id: String,
    pub canonical_root: String,
    pub display_root: String,
    pub git_common_dir: Option<String>,
    pub git_remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectAliasRecord {
    pub alias_path: String,
    pub project_id: String,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreInstanceUpsert {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct StoreInstanceRecord {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub created_at: i64,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphScopeUpsert {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct GraphScopeRecord {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreArtifactUpsert {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct StoreArtifactRecord {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectStoreResolution {
    pub project: CodeProjectRecord,
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectStoreContext {
    pub store: StoreInstanceRecord,
    pub graph_scopes: Vec<GraphScopeRecord>,
    pub artifacts: Vec<StoreArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRegistryContext {
    pub project: CodeProjectRecord,
    pub aliases: Vec<ProjectAliasRecord>,
    pub stores: Vec<ProjectStoreContext>,
}

/// Transcript-ingest backlog snapshot for a session store. See
/// the registered session-store health route.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionIngestHealth {
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
