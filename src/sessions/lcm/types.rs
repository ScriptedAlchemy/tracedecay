pub const MAX_DERIVED_TEXT_CHARS: usize = 64 * 1024;
pub const MAX_DERIVED_SNIPPET_CHARS: usize = 4 * 1024;
pub const DERIVED_TRUNCATION_MARKER: &str = "\n[derived snippet truncated by tokensave]";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmRawMessage {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content: String,
    pub content_hash: String,
    pub storage_kind: LcmStorageKind,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadRef {
    pub payload_ref: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: String,
    pub kind: String,
    pub content_hash: String,
    pub byte_count: u64,
    pub char_count: u64,
    pub created_at: i64,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadExpansion {
    pub payload_ref: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: String,
    pub content: String,
    pub offset: u64,
    pub char_count: u64,
    pub total_char_count: u64,
    pub byte_count: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmSourceRef {
    RawMessage { store_id: i64 },
    SummaryNode { node_id: String },
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryNode {
    pub node_id: String,
    pub provider: String,
    pub conversation_id: String,
    pub session_id: String,
    pub depth: i64,
    pub summary_text: String,
    pub summary_hash: String,
    pub source_refs: Vec<LcmSourceRef>,
    pub summary_token_count: i64,
    pub source_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmSummaryExpansion {
    pub summary: LcmSummaryNode,
    pub sources: Vec<LcmExpandedSummarySource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmContentSlice {
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmContentRange {
    pub offset: u64,
    pub limit: u64,
    pub returned_chars: u64,
    pub total_chars: u64,
    pub truncated: bool,
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
    pub request: LcmLoadSessionRequest,
    pub messages: Vec<LcmLoadSessionMessage>,
    pub next_cursor: Option<String>,
}

impl LcmLoadSessionPage {
    pub fn request_for_next(&self) -> LcmLoadSessionRequest {
        let mut request = self.request.clone();
        request.after_store_id = self
            .next_cursor
            .as_deref()
            .and_then(|cursor| cursor.parse::<i64>().ok());
        request
    }
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
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGrepHit {
    pub kind: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmExpandTarget {
    RawMessage { store_id: i64 },
    SummaryNode { node_id: String },
    ExternalPayload { payload_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandRequest {
    pub provider: String,
    pub session_id: String,
    pub target: LcmExpandTarget,
    pub content_slice: Option<LcmContentSlice>,
    /// Zero-based offset into a summary node's immediate source list
    /// (summary-node targets only). Mirrors hermes-lcm `lcm_expand`
    /// `source_offset`.
    #[serde(default)]
    pub source_offset: usize,
    /// Maximum number of immediate sources returned from `source_offset`
    /// (summary-node targets only). `None` returns all remaining sources,
    /// mirroring hermes-lcm `lcm_expand` `source_limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_limit: Option<usize>,
}

/// Pagination metadata for a summary node's immediate source list, mirroring
/// the hermes-lcm `lcm_expand` pagination payload (`_pagination_payload` in
/// `tools.py`). TokenSave slices each returned source by characters via
/// `content_slice` instead of sharing a token budget across sources, so the
/// resume cursor is `next_source_offset` alone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandSourcePagination {
    pub source_offset: usize,
    pub source_limit: usize,
    pub returned_sources: usize,
    pub total_sources: usize,
    pub next_source_offset: Option<usize>,
    pub has_more: bool,
    pub remaining_sources: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandResponse {
    pub kind: String,
    pub content: String,
    pub content_range: LcmContentRange,
    pub raw_message: Option<LcmRawMessage>,
    pub summary_node: Option<LcmSummaryNode>,
    pub summary_sources: Vec<LcmExpandedSummarySource>,
    pub payload_ref: Option<String>,
    /// Whether a raw-message target belongs to the requesting session.
    /// Mirrors hermes-lcm `from_current_session`; raw-message targets only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_current_session: Option<bool>,
    /// Set on cross-session raw-message expansions whose row references an
    /// externalized payload: payload bodies are session-scoped, so the ref
    /// is surfaced for traceability only. Mirrors hermes-lcm
    /// `externalized_note`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub externalized_note: Option<String>,
    /// Source-list pagination metadata (summary-node targets only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pagination: Option<LcmExpandSourcePagination>,
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
    pub prompt: String,
    pub query: Option<String>,
    pub answer: Option<String>,
    pub needs_synthesis: bool,
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
pub struct LcmSummaryNodeOverview {
    pub node_id: String,
    pub conversation_id: String,
    pub depth: i64,
    pub summary_preview: String,
    pub source_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmRawMessageOverview {
    pub message_id: String,
    pub store_id: i64,
    pub role: String,
    pub storage_kind: LcmStorageKind,
    pub payload_ref: Option<String>,
    pub content_preview: String,
    pub content_range: LcmContentRange,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmDescribeTarget {
    Session,
    SummaryNode { node_id: String },
    ExternalPayload { payload_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDescribeRequest {
    pub provider: String,
    pub session_id: String,
    pub target: LcmDescribeTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDescribeSourceOverview {
    pub source_kind: String,
    pub source_ref: LcmSourceRef,
    pub store_id: Option<i64>,
    pub node_id: Option<String>,
    pub role: Option<String>,
    pub storage_kind: Option<LcmStorageKind>,
    pub summary_token_count: Option<i64>,
    pub source_token_count: Option<i64>,
    pub expand_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDescribeSummaryNode {
    pub node_id: String,
    pub conversation_id: String,
    pub depth: i64,
    pub summary_token_count: i64,
    pub source_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
    pub source_count: usize,
    pub children: Vec<LcmDescribeSourceOverview>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDescribeExternalPayload {
    pub payload_ref: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: String,
    pub kind: String,
    pub content_hash: String,
    pub byte_count: u64,
    pub char_count: u64,
    pub created_at: i64,
    pub metadata_json: Option<String>,
    pub content_preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDescribeResponse {
    pub target: String,
    pub provider: String,
    pub session_id: String,
    pub raw_message_count: i64,
    pub summary_node_count: i64,
    pub external_payload_count: i64,
    pub first_store_id: Option<i64>,
    pub last_store_id: Option<i64>,
    pub raw_messages: Vec<LcmRawMessageOverview>,
    pub summary_nodes: Vec<LcmSummaryNodeOverview>,
    pub summary_node: Option<LcmDescribeSummaryNode>,
    pub external_payload: Option<LcmDescribeExternalPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmStatus {
    pub schema_version: i64,
    pub storage_scope: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmStoreStatus {
    pub messages: i64,
    pub estimated_tokens: i64,
}

/// Per-depth summary DAG counters mirroring the hermes-lcm `lcm_status`
/// `dag.depths` entries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmDagDepthStatus {
    pub count: i64,
    pub tokens: i64,
    pub source_tokens: i64,
}

/// Summary DAG diagnostics mirroring the hermes-lcm `lcm_status` `dag`
/// block: node/depth distribution and the source-to-summary compression
/// ratio rendered as `"N.N:1"` (`"0:1"` when the DAG is empty).
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
pub struct LcmPayloadStatus {
    pub externalized_count: i64,
    pub missing_count: i64,
    pub unreferenced_count: i64,
    pub placeholder_ref_count: i64,
    pub missing_placeholder_metadata_count: i64,
    pub missing_placeholder_file_count: i64,
    pub gc_candidate_count: i64,
    pub root_contained: bool,
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
pub struct LcmSummaryRequest {
    pub provider: String,
    pub session_id: String,
    pub focus_topic: Option<String>,
    pub prompt: String,
    pub source_range: LcmSummarySourceRange,
    pub source_messages: Vec<LcmSummarySourceMessage>,
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
    pub retry_status: Option<String>,
    pub frontier: LcmLifecycleState,
    pub summary_request: Option<LcmSummaryRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandedSummarySource {
    pub source_ref: LcmSourceRef,
    pub content: String,
    pub content_range: Option<LcmContentRange>,
    pub raw_message: Option<LcmRawMessage>,
    pub summary_node: Option<Box<LcmSummaryNode>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcmError {
    InvalidPayloadRef,
    PayloadNotFound,
    PayloadNotOwnedBySession,
    PayloadMissing,
    PayloadIntegrityMismatch,
    SummaryNodeNotFound,
    SummarySourceNotOwnedBySession,
    Db(String),
    Io(String),
}

impl std::fmt::Display for LcmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPayloadRef => write!(f, "invalid payload ref"),
            Self::PayloadNotFound => write!(f, "payload not found"),
            Self::PayloadNotOwnedBySession => write!(f, "payload not owned by session"),
            Self::PayloadMissing => write!(f, "payload file missing"),
            Self::PayloadIntegrityMismatch => write!(f, "payload integrity mismatch"),
            Self::SummaryNodeNotFound => write!(f, "summary node not found"),
            Self::SummarySourceNotOwnedBySession => {
                write!(f, "summary source not owned by session")
            }
            Self::Db(message) => write!(f, "payload database error: {message}"),
            Self::Io(message) => write!(f, "payload IO error: {message}"),
        }
    }
}

impl std::error::Error for LcmError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmStorageKind {
    Inline,
    External,
}

impl LcmStorageKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::External => "external",
        }
    }

    pub(crate) fn from_db(value: &str) -> Option<Self> {
        match value {
            "inline" => Some(Self::Inline),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}
