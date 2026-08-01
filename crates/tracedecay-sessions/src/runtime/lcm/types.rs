//! LCM value types owned by the session store.
//!
//! The DB-free retrieval and rendering contracts live in
//! [`crate::application::session::lcm`] and are re-exported here so
//! `sessions::lcm` remains the single import surface for the session engine.
//! Only infrastructure-facing conversions — notably the SQL error mapping —
//! stay in this module.

pub use crate::application::session::compatibility::{
    DERIVED_TRUNCATION_MARKER, MAX_DERIVED_SNIPPET_CHARS, MAX_DERIVED_TEXT_CHARS,
};
pub use crate::application::session::lcm::contracts::{
    LcmContentRange, LcmContentSlice, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmPayloadExpansion, LcmPayloadRef, LcmRawMessage,
    LcmRawMessageOverview, LcmSourceRef, LcmStorageKind, LcmSummaryNode, LcmSummaryNodeOverview,
};

impl From<tracedecay_runtime_core::db::engine::Error> for LcmError {
    fn from(err: tracedecay_runtime_core::db::engine::Error) -> Self {
        Self::Db(err.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryNodeDraft {
    pub provider: String,
    pub conversation_id: String,
    pub session_id: String,
    pub depth: i64,
    pub summary_text: String,
    pub source_refs: Vec<LcmSourceRef>,
    pub source_token_count: i64,
    pub summary_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
}

/// Explicit identity and predecessor edge for one immutable summary
/// publication. `draft` is also materialized into the legacy LCM tables as a
/// compatibility projection in the authoritative transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcmImmutableSummaryPublication {
    pub summary_id: String,
    pub predecessor_summary_id: Option<String>,
    pub draft: LcmSummaryNodeDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LcmSummaryPublicationDisposition {
    Published,
    ExactReplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcmSummaryPublicationReceipt {
    pub summary: LcmSummaryNode,
    pub disposition: LcmSummaryPublicationDisposition,
    pub generation: i64,
    pub frozen_watermarks_json: String,
    pub published_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryExpansion {
    pub summary: LcmSummaryNode,
    pub sources: Vec<LcmExpandedSummarySource>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLoadSessionRequest {
    pub provider: String,
    pub session_id: String,
    pub after_store_id: Option<i64>,
    pub limit: usize,
    pub roles: Vec<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub content_slice: Option<LcmContentSlice>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLoadSessionPage {
    pub messages: Vec<LcmLoadSessionMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLoadSessionMessage {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content: String,
    pub content_range: LcmContentRange,
    pub content_hash: String,
    pub storage_kind: LcmStorageKind,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

/// Recency-ordered overview of one session in the LCM raw store, used to
/// select "recently active" sessions for automation replay evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmRecentSession {
    pub provider: String,
    pub session_id: String,
    pub message_count: i64,
    pub first_timestamp: Option<i64>,
    pub last_timestamp: Option<i64>,
    pub last_store_id: i64,
}

/// Bounded turn-ordered replay slice request for one session: up to
/// `head_limit` opening turns, `tail_limit` closing turns, and
/// `summary_limit` summary-DAG nodes, each snippet capped by chars.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSessionReplayRequest {
    pub provider: String,
    pub session_id: String,
    pub head_limit: usize,
    pub tail_limit: usize,
    pub max_snippet_chars: usize,
    pub summary_limit: usize,
    pub max_summary_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSessionReplaySlice {
    pub provider: String,
    pub session_id: String,
    pub total_messages: i64,
    /// Messages between the head and tail slices that were not included.
    pub omitted_messages: i64,
    pub head: Vec<LcmReplayMessage>,
    pub tail: Vec<LcmReplayMessage>,
    pub summary_nodes: Vec<LcmReplaySummaryNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmReplayMessage {
    pub message_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub snippet: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmReplaySummaryNode {
    pub node_id: String,
    pub depth: i64,
    pub created_at: i64,
    pub snippet: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmScope {
    Current,
    Session,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmGrepSort {
    Recency,
    Relevance,
    Hybrid,
}

impl std::str::FromStr for LcmGrepSort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "recency" => Ok(Self::Recency),
            "relevance" => Ok(Self::Relevance),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGrepRequest {
    pub provider: String,
    pub query: String,
    pub scope: LcmScope,
    pub session_id: Option<String>,
    pub include_summaries: bool,
    pub limit: usize,
    pub sort: LcmGrepSort,
    pub source: Option<String>,
    pub role: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    /// Optional git-scope constraint (branch/worktree/commit). When set,
    /// hits are limited to sessions correlated with the git ref via EXISTS
    /// pushdown against the git-correlation tables. Defaults to `None`.
    #[serde(
        default,
        skip_serializing_if = "crate::runtime::git_correlation::GitScopeFilter::is_empty"
    )]
    pub git_filter: crate::runtime::git_correlation::GitScopeFilter,
}

/// Query-only filters layered over the raw LCM request. Kept separate so
/// compression/replay callers retain their existing request construction while
/// interactive retrieval can select parent/subagent and semantic message type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LcmGrepFilters {
    pub relationship_scope: crate::runtime::SessionSearchScope,
    pub message_type: crate::runtime::SessionMessageType,
}

impl Default for LcmGrepFilters {
    fn default() -> Self {
        Self {
            relationship_scope: crate::runtime::SessionSearchScope::All,
            message_type: crate::runtime::SessionMessageType::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGrepHit {
    pub kind: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    /// Raw-message role (`assistant`/`user`/`tool`/`system`); `None` for
    /// summary nodes and rows ingested before roles were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub snippet: String,
}

/// Grep hits plus retrieval-policy disclosure: sessions whose matches were
/// dropped by the cross-session per-session cap, with dropped-hit counts.
/// Silent truncation reads as "covered everything"; callers must be able to
/// see that a session has more matches and rerun with `scope=session`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGrepOutcome {
    pub hits: Vec<LcmGrepHit>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub capped_sessions: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryRequest {
    pub provider: String,
    pub session_id: String,
    pub prompt: String,
    pub query: Option<String>,
    pub node_ids: Vec<String>,
    pub max_results: usize,
    pub max_tokens: usize,
    pub context_max_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryResponse {
    // `answer` and `needs_synthesis` lead the payload so the synthesized answer
    // renders first in both JSON and generic markdown output.
    pub answer: Option<String>,
    pub needs_synthesis: bool,
    pub prompt: String,
    pub query: Option<String>,
    pub synthesis_prompt: Option<LcmExpandQuerySynthesisPrompt>,
    pub max_tokens: usize,
    pub context_max_tokens: usize,
    pub context_budget: LcmExpandQueryBudget,
    pub context_truncated: bool,
    pub context_pagination: Vec<LcmExpandQueryPagination>,
    pub node_ids: Vec<String>,
    pub matches: Vec<LcmExpandQueryMatch>,
    pub context_blocks: Vec<LcmExpandQueryContextBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQuerySynthesisPrompt {
    pub system: String,
    pub user: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryBudget {
    pub requested_max_chars: usize,
    pub used_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryPagination {
    pub kind: String,
    pub node_id: Option<String>,
    pub source_ref: Option<LcmSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<tracedecay_domain::HydrationStateV1>,
    pub next_content_offset: Option<u64>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryMatch {
    pub kind: String,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandQueryContextBlock {
    pub kind: String,
    pub node_id: Option<String>,
    pub source_ref: Option<LcmSourceRef>,
    pub content: String,
    pub content_range: LcmContentRange,
    pub raw_message: Option<LcmRawMessage>,
    pub summary_node: Option<LcmSummaryNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmStatus {
    pub schema_version: i64,
    pub raw_message_count: i64,
    pub summary_node_count: i64,
    pub external_payload_count: i64,
    pub missing_payload_count: i64,
    pub unreferenced_payload_count: i64,
    pub maintenance_debt_count: i64,
    pub store: LcmStoreStatus,
    pub dag: LcmDagStatus,
    pub config: LcmConfigStatus,
    pub payload: LcmPayloadStatus,
    pub payload_gc: LcmPayloadGcStatus,
    pub lifecycle: LcmLifecycleStatus,
    pub redaction: LcmRedactionStatus,
}

/// Default fresh-tail size applied when the host omits `fresh_tail_count`.
/// Mirrors `compression.rs` `DEFAULT_FRESH_TAIL_COUNT`; keep in sync.
pub const LCM_DEFAULT_FRESH_TAIL_COUNT: usize = 2;

/// Default condensation fan-in applied when the host omits `summary_fan_in`.
/// Mirrors `compression.rs` `DEFAULT_SUMMARY_FAN_IN`; keep in sync.
pub const LCM_DEFAULT_SUMMARY_FAN_IN: usize = 4;

/// Compression-boundary skip cooldown in seconds. Mirrors `compression.rs`
/// `COMPRESSION_BOUNDARY_COOLDOWN_SECONDS`; keep in sync.
pub const LCM_COMPRESSION_BOUNDARY_COOLDOWN_SECONDS: i64 = 60;

/// Raw-store size diagnostics mirroring the hermes-lcm `lcm_status` `store`
/// block. `estimated_tokens` uses the engine's deterministic whitespace
/// token estimate over stored message content.
///
/// `messages` is the exact row count. The token estimate has to read every
/// message body, which on a multi-gigabyte profile store cannot finish inside
/// a request deadline, so `token_estimate` states how much of the store the
/// reported estimate actually covers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmStoreStatus {
    pub messages: i64,
    pub estimated_tokens: i64,
    pub token_estimate: LcmStoreTokenCoverage,
}

/// Coverage of the raw-store token estimate.
///
/// A partial estimate is a typed state, never a smaller number presented as
/// the whole store: `complete` is false, `scanned_messages` reports how many
/// bodies were summed, and `next_after_store_id` resumes the scan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmStoreTokenCoverage {
    pub complete: bool,
    pub scanned_messages: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after_store_id: Option<i64>,
}

impl LcmStoreTokenCoverage {
    pub const fn complete(scanned_messages: i64) -> Self {
        Self {
            complete: true,
            scanned_messages,
            next_after_store_id: None,
        }
    }
}

/// Per-depth summary counters mirroring the hermes-lcm `lcm_status`
/// `dag.depths` entries. Most LCM producers use this as DAG lineage depth;
/// Codex compaction summaries store compaction generation in the same field.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDagDepthStatus {
    pub count: i64,
    pub tokens: i64,
    pub source_tokens: i64,
}

/// Summary diagnostics mirroring the hermes-lcm `lcm_status` `dag` block:
/// node/depth distribution and the source-to-summary compression ratio rendered
/// as `"N.N:1"` (`"0:1"` when the DAG is empty).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDagStatus {
    pub total_nodes: i64,
    pub total_tokens: i64,
    pub total_source_tokens: i64,
    pub compression_ratio: String,
    pub depths: std::collections::BTreeMap<String, LcmDagDepthStatus>,
}

/// Effective engine defaults applied when the stateless host omits the
/// corresponding knobs, mirroring the hermes-lcm `lcm_status` `config`
/// block. Per-call host overrides are not visible to this storage-side
/// status report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmConfigStatus {
    pub fresh_tail_count: usize,
    pub summary_fan_in: usize,
    pub compression_boundary_cooldown_seconds: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmCleanConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stateless_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_message_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGcConfig {
    #[serde(
        default = "default_lcm_gc_grace_seconds",
        deserialize_with = "deserialize_lcm_gc_grace_seconds"
    )]
    pub grace_seconds: u64,
    #[serde(default = "default_lcm_gc_reap_missing_after")]
    pub reap_missing_after: u64,
    #[serde(default = "default_lcm_gc_reap_missing_enabled")]
    pub reap_missing_enabled: bool,
    #[serde(default = "default_lcm_gc_max_batch_size")]
    pub max_batch_size: usize,
    #[serde(default = "default_lcm_gc_backup_before_reap")]
    pub backup_before_reap: bool,
    #[serde(default = "default_lcm_gc_interval_seconds")]
    pub interval_seconds: u64,
    #[serde(default = "default_lcm_gc_enabled")]
    pub gc_enabled: bool,
}

impl LcmGcConfig {
    pub const MIN_GRACE_SECONDS: u64 = 300;

    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.grace_seconds = Self::clamp_grace_seconds(self.grace_seconds);
        self
    }

    fn clamp_grace_seconds(value: u64) -> u64 {
        value.max(Self::MIN_GRACE_SECONDS)
    }
}

impl Default for LcmGcConfig {
    fn default() -> Self {
        Self {
            grace_seconds: default_lcm_gc_grace_seconds(),
            reap_missing_after: default_lcm_gc_reap_missing_after(),
            reap_missing_enabled: default_lcm_gc_reap_missing_enabled(),
            max_batch_size: default_lcm_gc_max_batch_size(),
            backup_before_reap: default_lcm_gc_backup_before_reap(),
            interval_seconds: default_lcm_gc_interval_seconds(),
            gc_enabled: default_lcm_gc_enabled(),
        }
        .normalized()
    }
}

fn default_lcm_gc_grace_seconds() -> u64 {
    86_400
}

fn default_lcm_gc_reap_missing_after() -> u64 {
    604_800
}

fn default_lcm_gc_reap_missing_enabled() -> bool {
    false
}

fn default_lcm_gc_max_batch_size() -> usize {
    500
}

fn default_lcm_gc_backup_before_reap() -> bool {
    true
}

fn default_lcm_gc_interval_seconds() -> u64 {
    21_600
}

fn default_lcm_gc_enabled() -> bool {
    true
}

fn deserialize_lcm_gc_grace_seconds<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<u64> as serde::Deserialize>::deserialize(deserializer)?
        .unwrap_or_else(default_lcm_gc_grace_seconds);
    Ok(LcmGcConfig::clamp_grace_seconds(value))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadGcStatus {
    pub last_gc_at: Option<i64>,
    pub last_gc_duration_ms: Option<u64>,
    pub last_gc_status: Option<String>,
    pub last_gc_error: Option<String>,
    pub last_reaped_refs: Option<i64>,
    pub last_reaped_bytes: Option<u64>,
    pub grace_seconds: i64,
    pub reap_missing_metadata_after_seconds: i64,
    pub next_run_eligible_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmPayloadCoverageState {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadCoverage {
    pub state: LcmPayloadCoverageState,
    pub scanned_metadata_refs: i64,
    pub scanned_files: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadStatus {
    pub coverage: LcmPayloadCoverage,
    pub externalized_count: i64,
    pub missing_count: i64,
    pub unreferenced_count: i64,
    pub placeholder_ref_count: i64,
    pub missing_placeholder_metadata_count: i64,
    pub missing_placeholder_file_count: i64,
    pub gc_candidate_count: i64,
    pub root_contained: bool,
    pub orphan_file_count: i64,
    pub tombstoned_count: i64,
    pub referenced_count: i64,
    pub total_bytes: u64,
    pub referenced_bytes: u64,
    pub orphan_file_bytes: u64,
    pub reclaimable_bytes: u64,
    pub reclaimable_bytes_after_grace: u64,
    pub integrity_mismatch_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLifecycleStatus {
    pub lifecycle_state_count: i64,
    pub frontier_count: i64,
    pub maintenance_debt_count: i64,
    pub current_session_id: Option<String>,
    pub current_frontier_store_id: Option<i64>,
    pub last_finalized_session_id: Option<String>,
    pub last_finalized_frontier_store_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmRedactionStatus {
    pub enabled: bool,
    pub lossy_records: i64,
    pub legacy_truncated_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLifecycleUpdate {
    pub provider: String,
    pub conversation_id: String,
    pub current_session_id: String,
    pub current_frontier_store_id: Option<i64>,
    pub last_finalized_session_id: Option<String>,
    pub last_finalized_frontier_store_id: Option<i64>,
    pub maintenance_debt: Vec<LcmMaintenanceDebt>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmLifecycleState {
    pub provider: String,
    pub conversation_id: String,
    pub current_session_id: String,
    pub current_frontier_store_id: Option<i64>,
    pub last_finalized_session_id: Option<String>,
    pub last_finalized_frontier_store_id: Option<i64>,
    pub maintenance_debt: Vec<LcmMaintenanceDebt>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmMaintenanceDebt {
    RawBacklog {
        from_store_id: i64,
        to_store_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPreflightRequest {
    pub provider: String,
    pub session_id: String,
    pub messages: Vec<serde_json::Value>,
    pub current_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_assembly_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_chunk_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_messages: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_fan_in: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental_max_depth: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_tail_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_leaf_chunk_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_leaf_chunk_max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens_floor: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stateless_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_message_patterns: Vec<String>,
}

/// Host notification that a session crossed a compression boundary.
///
/// Mirrors the hermes-lcm `on_session_start(..., boundary_reason="compression",
/// old_session_id=...)` contract: when the old session does not match the
/// host's bound session the boundary skipped carry-over and a short compression
/// cooldown starts for the new session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSessionBoundaryRequest {
    pub provider: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_session_id: Option<String>,
    /// Unix timestamp of the boundary event; defaults to now when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary_skip_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSessionBoundaryResponse {
    pub status: String,
    pub recorded: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPreflightResponse {
    pub status: String,
    pub should_compress: bool,
    pub reason: String,
    pub replay_messages: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummarySourceRange {
    pub from_store_id: i64,
    pub to_store_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummarySourceMessage {
    pub store_id: i64,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExtractionRequest {
    pub session_id: String,
    pub source_range: LcmSummarySourceRange,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExtractionResult {
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryRequest {
    pub provider: String,
    pub session_id: String,
    pub focus_topic: Option<String>,
    pub prompt: String,
    pub source_range: LcmSummarySourceRange,
    pub source_messages: Vec<LcmSummarySourceMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction_request: Option<LcmExtractionRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmCompressionResponse {
    pub status: String,
    pub reason: String,
    pub summary_nodes_created: usize,
    pub summary_nodes: Vec<LcmSummaryNode>,
    pub replay_messages: Vec<serde_json::Value>,
    pub replay_token_estimate: i64,
    pub replay_over_budget: bool,
    pub compression_attempts: usize,
    pub fallback_used: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_recovery_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_status: Option<String>,
    pub frontier: LcmLifecycleState,
    pub summary_request: Option<LcmSummaryRequest>,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn gc_config_clamps_grace_floor_from_serde() {
        let config: LcmGcConfig = serde_json::from_str(
            r#"{"grace_seconds":10,"reap_missing_after":0,"gc_enabled":false}"#,
        )
        .expect("gc config should deserialize");

        assert_eq!(config.grace_seconds, 300);
        assert_eq!(config.reap_missing_after, 0);
        assert!(!config.gc_enabled);
    }

    #[test]
    fn gc_config_defaults_match_spec() {
        let config = LcmGcConfig::default();

        assert_eq!(config.grace_seconds, 86_400);
        assert_eq!(config.reap_missing_after, 604_800);
        assert!(!config.reap_missing_enabled);
        assert_eq!(config.max_batch_size, 500);
        assert!(config.backup_before_reap);
        assert_eq!(config.interval_seconds, 21_600);
        assert!(config.gc_enabled);
    }

    #[test]
    fn gc_config_round_trips_with_serde_defaults() {
        let config: LcmGcConfig =
            serde_json::from_str("{}").expect("empty gc config should deserialize with defaults");
        let value = serde_json::to_value(&config).expect("gc config should serialize");

        assert_eq!(value["grace_seconds"], 86_400);
        assert_eq!(value["reap_missing_after"], 604_800);
        assert_eq!(value["reap_missing_enabled"], false);
        assert_eq!(value["max_batch_size"], 500);
        assert_eq!(value["backup_before_reap"], true);
        assert_eq!(value["interval_seconds"], 21_600);
        assert_eq!(value["gc_enabled"], true);
    }

    #[test]
    fn gc_config_reap_missing_zero_means_never() {
        let config: LcmGcConfig =
            serde_json::from_str(r#"{"reap_missing_enabled":true,"reap_missing_after":0}"#)
                .expect("missing reap config should deserialize");

        assert!(config.reap_missing_enabled);
        assert_eq!(config.reap_missing_after, 0);
    }

    #[test]
    fn lcm_error_display_includes_gc_variants() {
        assert_eq!(
            LcmError::PayloadGcd.to_string(),
            "payload already garbage collected"
        );
        assert_eq!(
            LcmError::StillReferenced.to_string(),
            "payload still referenced"
        );
    }
}
