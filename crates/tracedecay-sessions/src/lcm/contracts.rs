//! DB-free LCM retrieval contracts shared by the session store and the
//! registered temporal adapters.
//!
//! These are value types only. Nothing here opens a connection, holds a
//! snapshot, or touches the filesystem, so both the session LCM engine and the
//! global-database adapters can depend on them without a reciprocal edge.

use std::path::{Component, Path};

use tracedecay_domain::HydrationStateV1;

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
pub struct LcmRawMessageMetadata {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content_hash: String,
    pub storage_kind: LcmStorageKind,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

impl LcmRawMessage {
    pub fn into_metadata(self) -> LcmRawMessageMetadata {
        LcmRawMessageMetadata {
            provider: self.provider,
            message_id: self.message_id,
            session_id: self.session_id,
            store_id: self.store_id,
            role: self.role,
            ordinal: self.ordinal,
            timestamp: self.timestamp,
            content_hash: self.content_hash,
            storage_kind: self.storage_kind,
            payload_ref: self.payload_ref,
            legacy_source: self.legacy_source,
            legacy_truncated: self.legacy_truncated,
            metadata_json: self.metadata_json,
        }
    }
}

impl LcmRawMessageMetadata {
    pub fn with_verified_content(self, content: String) -> Result<LcmRawMessage, LcmError> {
        if crate::compatibility::projected_content_hash(&content) != self.content_hash {
            return Err(LcmError::PayloadIntegrityMismatch);
        }
        Ok(self.with_content(content))
    }

    pub(crate) fn with_external_placeholder(
        self,
        content: String,
    ) -> Result<LcmRawMessage, LcmError> {
        if self.storage_kind != LcmStorageKind::External {
            return Err(LcmError::PayloadIntegrityMismatch);
        }
        Ok(self.with_content(content))
    }

    fn with_content(self, content: String) -> LcmRawMessage {
        LcmRawMessage {
            provider: self.provider,
            message_id: self.message_id,
            session_id: self.session_id,
            store_id: self.store_id,
            role: self.role,
            ordinal: self.ordinal,
            timestamp: self.timestamp,
            content,
            content_hash: self.content_hash,
            storage_kind: self.storage_kind,
            payload_ref: self.payload_ref,
            legacy_source: self.legacy_source,
            legacy_truncated: self.legacy_truncated,
            metadata_json: self.metadata_json,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LcmDataFreshness {
    Fresh,
    Stored { generation_lag: u64 },
    Partial { generation_lag: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LcmRetrievalOutcome {
    Complete {
        freshness: LcmDataFreshness,
    },
    Partial {
        freshness: LcmDataFreshness,
        omitted: u64,
    },
    Stale {
        freshness: LcmDataFreshness,
    },
}

impl LcmRetrievalOutcome {
    pub const fn complete(freshness: LcmDataFreshness) -> Self {
        Self::Complete { freshness }
    }

    pub const fn partial(freshness: LcmDataFreshness, omitted: u64) -> Self {
        Self::Partial { freshness, omitted }
    }

    pub const fn stale(freshness: LcmDataFreshness) -> Self {
        Self::Stale { freshness }
    }

    pub const fn freshness(self) -> LcmDataFreshness {
        match self {
            Self::Complete { freshness }
            | Self::Partial { freshness, .. }
            | Self::Stale { freshness } => freshness,
        }
    }

    pub const fn omitted(self) -> u64 {
        match self {
            Self::Partial { omitted, .. } => omitted,
            Self::Complete { .. } | Self::Stale { .. } => 0,
        }
    }
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
    /// Internal zero-based boundary decoded from an authenticated cursor.
    /// Never serialized as a caller-controlled continuation.
    #[serde(skip)]
    pub source_offset: usize,
    /// Maximum number of immediate sources returned from the authenticated
    /// boundary. `None` returns all remaining sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_limit: Option<usize>,
}

/// Public pagination metadata for a summary node's immediate source list.
/// Numeric page boundaries are serialization-private; external callers resume
/// only with the authenticated `next_cursor` emitted by the temporal service.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandSourcePagination {
    /// Internal page boundary used while rendering a cursor-authenticated
    /// request. Never serialized: callers continue only with `next_cursor`.
    #[serde(skip)]
    pub source_offset: usize,
    pub source_limit: usize,
    pub returned_sources: usize,
    pub total_sources: usize,
    /// Internal next boundary consumed by the cursor encoder. Never serialized
    /// because an unauthenticated numeric continuation is not a public API.
    #[serde(skip)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message_metadata: Option<LcmRawMessageMetadata>,
    pub summary_node: Option<LcmSummaryNode>,
    pub summary_sources: Vec<LcmExpandedSummarySource>,
    pub payload_ref: Option<String>,
    /// Whether a raw-message target belongs to the requesting session.
    /// Mirrors hermes-lcm `from_current_session`; raw-message targets only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_current_session: Option<bool>,
    /// Legacy compatibility note mirrored from hermes-lcm payloads. Modern
    /// cross-session expansion flows should rely on `payload_ref` +
    /// `raw_message.session_id` and remain note-free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub externalized_note: Option<String>,
    /// Source-list coverage metadata (summary-node targets only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pagination: Option<LcmExpandSourcePagination>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmExpandedSummarySource {
    pub source_ref: LcmSourceRef,
    #[serde(default = "default_summary_source_hydration_state")]
    pub state: HydrationStateV1,
    pub content: String,
    pub content_range: Option<LcmContentRange>,
    #[serde(default)]
    pub content_truncated: bool,
    pub raw_message: Option<LcmRawMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message_metadata: Option<LcmRawMessageMetadata>,
    pub summary_node: Option<Box<LcmSummaryNode>>,
}

const fn default_summary_source_hydration_state() -> HydrationStateV1 {
    HydrationStateV1::RetainedButUnavailable
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcmStorageKind {
    Inline,
    External,
}

impl LcmStorageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::External => "external",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "inline" => Some(Self::Inline),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

/// Reject any payload reference that is not a single normal path component.
///
/// Containment is decided on the reference itself, before any storage root is
/// joined, so every caller — session store, registered renderer, hydration —
/// rejects traversal identically.
pub fn validate_payload_ref(payload_ref: &str) -> Result<&str, LcmError> {
    if payload_ref.is_empty()
        || payload_ref == "."
        || payload_ref == ".."
        || payload_ref.contains('/')
        || payload_ref.contains('\\')
    {
        return Err(LcmError::InvalidPayloadRef);
    }

    let mut components = Path::new(payload_ref).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(payload_ref),
        _ => Err(LcmError::InvalidPayloadRef),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcmError {
    ProfileResetRequired {
        found_version: Option<i64>,
        required_version: i64,
    },
    InvalidPayloadRef,
    PayloadNotFound,
    PayloadNotOwnedBySession,
    PayloadMissing,
    PayloadGcd,
    PayloadLocked,
    PayloadIntegrityMismatch,
    StillReferenced,
    SummaryNodeNotFound,
    SummarySourceNotOwnedBySession,
    ImmutableSummaryConflict {
        summary_id: String,
    },
    ImmutablePayloadConflict {
        payload_ref: String,
    },
    SummaryPredecessorRequired {
        summary_id: String,
        current_predecessor_id: String,
    },
    InvalidSummarySuccessor {
        summary_id: String,
        predecessor_summary_id: String,
    },
    SummaryCycle {
        summary_id: String,
    },
    SummarySourceUnavailable {
        source_id: String,
        reason: String,
    },
    StaleSummaryGeneration {
        expected: i64,
        actual: i64,
    },
    LifecycleStateNotFound,
    Cancelled,
    DeadlineExceeded,
    BudgetExhausted,
    Db(String),
    Io(String),
}

impl std::fmt::Display for LcmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProfileResetRequired {
                found_version,
                required_version,
            } => match found_version {
                Some(found_version) => write!(
                    f,
                    "LCM profile schema {found_version} is incompatible with required schema \
                     {required_version}; reset the profile"
                ),
                None => write!(
                    f,
                    "unversioned LCM profile data is incompatible with required schema \
                     {required_version}; reset the profile"
                ),
            },
            Self::InvalidPayloadRef => write!(f, "invalid payload ref"),
            Self::PayloadNotFound => write!(f, "payload not found"),
            Self::PayloadNotOwnedBySession => write!(f, "payload not owned by session"),
            Self::PayloadMissing => write!(f, "payload file missing"),
            Self::PayloadGcd => write!(f, "payload already garbage collected"),
            Self::PayloadLocked => write!(f, "payload is locked by quarantine policy"),
            Self::PayloadIntegrityMismatch => write!(f, "payload integrity mismatch"),
            Self::StillReferenced => write!(f, "payload still referenced"),
            Self::SummaryNodeNotFound => write!(f, "summary node not found"),
            Self::SummarySourceNotOwnedBySession => {
                write!(f, "summary source not owned by session")
            }
            Self::ImmutableSummaryConflict { summary_id } => {
                write!(
                    f,
                    "immutable summary {summary_id} conflicts with its publication"
                )
            }
            Self::ImmutablePayloadConflict { payload_ref } => {
                write!(
                    f,
                    "immutable payload {payload_ref} conflicts with its manifest"
                )
            }
            Self::SummaryPredecessorRequired {
                summary_id,
                current_predecessor_id,
            } => write!(
                f,
                "summary {summary_id} must name current predecessor {current_predecessor_id}"
            ),
            Self::InvalidSummarySuccessor {
                summary_id,
                predecessor_summary_id,
            } => write!(
                f,
                "summary {summary_id} cannot succeed incompatible predecessor \
                 {predecessor_summary_id}"
            ),
            Self::SummaryCycle { summary_id } => {
                write!(f, "summary {summary_id} would create a lineage cycle")
            }
            Self::SummarySourceUnavailable { source_id, reason } => {
                write!(f, "summary source {source_id} is unavailable: {reason}")
            }
            Self::StaleSummaryGeneration { expected, actual } => {
                write!(
                    f,
                    "summary generation compare-and-swap failed: expected {expected}, actual {actual}"
                )
            }
            Self::LifecycleStateNotFound => {
                write!(f, "payload database error: lifecycle state not found")
            }
            Self::Cancelled => write!(f, "LCM payload verification was cancelled"),
            Self::DeadlineExceeded => {
                write!(f, "LCM payload verification deadline was exceeded")
            }
            Self::BudgetExhausted => {
                write!(f, "LCM payload verification budget was exhausted")
            }
            Self::Db(message) => write!(f, "payload database error: {message}"),
            Self::Io(message) => write!(f, "payload IO error: {message}"),
        }
    }
}

impl std::error::Error for LcmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_outcomes_serialize_freshness_and_omitted_counts() {
        let stale = serde_json::to_value(LcmRetrievalOutcome::stale(LcmDataFreshness::Stored {
            generation_lag: 7,
        }))
        .expect("serialize stale outcome");
        assert_eq!(stale["outcome"], "stale");
        assert_eq!(stale["freshness"]["state"], "stored");
        assert_eq!(stale["freshness"]["generation_lag"], 7);

        let partial = serde_json::to_value(LcmRetrievalOutcome::partial(
            LcmDataFreshness::Partial { generation_lag: 3 },
            5,
        ))
        .expect("serialize partial outcome");
        assert_eq!(partial["outcome"], "partial");
        assert_eq!(partial["freshness"]["state"], "partial");
        assert_eq!(partial["freshness"]["generation_lag"], 3);
        assert_eq!(partial["omitted"], 5);
    }
}
