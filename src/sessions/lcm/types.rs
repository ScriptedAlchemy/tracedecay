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
    pub role: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmGrepRequest {
    pub provider: String,
    pub query: String,
    pub scope: LcmScope,
    pub session_id: Option<String>,
    pub include_summaries: bool,
    pub limit: usize,
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
pub struct LcmDescribeResponse {
    pub provider: String,
    pub session_id: String,
    pub raw_message_count: i64,
    pub summary_node_count: i64,
    pub external_payload_count: i64,
    pub first_store_id: Option<i64>,
    pub last_store_id: Option<i64>,
    pub raw_messages: Vec<LcmRawMessageOverview>,
    pub summary_nodes: Vec<LcmSummaryNodeOverview>,
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
    pub payload: LcmPayloadStatus,
    pub lifecycle: LcmLifecycleStatus,
    pub redaction: LcmRedactionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmPayloadStatus {
    pub externalized_count: i64,
    pub missing_count: i64,
    pub unreferenced_count: i64,
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
