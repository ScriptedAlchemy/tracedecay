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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandedSummarySource {
    pub source_ref: LcmSourceRef,
    pub content: String,
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
