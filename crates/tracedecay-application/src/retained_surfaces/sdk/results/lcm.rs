use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    HydrationStateResultV1, RetainedErrorV1, RetainedOutcomeStatusV1, TemporalCoverageV1,
    TemporalExplanationV1, TemporalOmissionV1, TemporalWatermarksV1,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LcmAuthorityOutcomeV1 {
    Ready,
    Denied,
    Cancelled,
    TimedOut,
    Unavailable { reason: String },
    Failed { diagnostic: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmStatusV1 {
    pub schema_version: i64,
    pub raw_message_count: i64,
    pub summary_node_count: i64,
    pub external_payload_count: i64,
    pub missing_payload_count: i64,
    pub unreferenced_payload_count: i64,
    pub maintenance_debt_count: i64,
    pub store: LcmStoreStatusV1,
    pub dag: LcmDagStatusV1,
    pub config: LcmConfigStatusV1,
    pub payload: LcmPayloadStatusV1,
    pub payload_gc: LcmPayloadGcStatusV1,
    pub lifecycle: LcmLifecycleStatusV1,
    pub redaction: LcmRedactionStatusV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmStoreStatusV1 {
    pub messages: i64,
    pub estimated_tokens: i64,
    pub token_estimate: LcmStoreTokenCoverageV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmStoreTokenCoverageV1 {
    pub complete: bool,
    pub scanned_messages: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_store_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDagDepthStatusV1 {
    pub count: i64,
    pub tokens: i64,
    pub source_tokens: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDagStatusV1 {
    pub total_nodes: i64,
    pub total_tokens: i64,
    pub total_source_tokens: i64,
    pub compression_ratio: String,
    pub depths: BTreeMap<String, LcmDagDepthStatusV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmConfigStatusV1 {
    pub fresh_tail_count: usize,
    pub summary_fan_in: usize,
    pub compression_boundary_cooldown_seconds: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmPayloadGcStatusV1 {
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmPayloadCoverageStateV1 {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmPayloadCoverageV1 {
    pub state: LcmPayloadCoverageStateV1,
    pub scanned_metadata_refs: i64,
    pub scanned_files: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmPayloadStatusV1 {
    pub coverage: LcmPayloadCoverageV1,
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmLifecycleStatusV1 {
    pub lifecycle_state_count: i64,
    pub frontier_count: i64,
    pub maintenance_debt_count: i64,
    pub current_session_id: Option<String>,
    pub current_frontier_store_id: Option<i64>,
    pub last_finalized_session_id: Option<String>,
    pub last_finalized_frontier_store_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmRedactionStatusV1 {
    pub enabled: bool,
    pub lossy_records: i64,
    pub legacy_truncated_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmStatusResultV1 {
    pub status: RetainedOutcomeStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_outcome: Option<LcmAuthorityOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lcm: Option<LcmStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmDoctorHealthStatusV1 {
    Complete,
    Partial,
    Unavailable,
    Locked,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmDoctorFindingKindV1 {
    TriggerAuditDrift,
    OccurrenceFtsCorruption,
    SummaryFtsCorruption,
    MissingAnchor,
    MissingReceipt,
    InvalidGeneration,
    MultiActiveGeneration,
    CursorChainAbsent,
    CursorKeyAbsent,
    OwnershipDrift,
    StuckRefresh,
    StuckBinding,
    StuckProgress,
    StuckReceipt,
    MigrationGap,
    CompatibilityDrift,
    RelationGraphUnavailable,
    RelationGraphCorruption,
    RelationGraphCycle,
    StaleSummaryClosure,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDoctorFindingV1 {
    pub kind: LcmDoctorFindingKindV1,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDoctorHealthV1 {
    pub status: LcmDoctorHealthStatusV1,
    pub findings: Vec<LcmDoctorFindingV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDoctorResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub authority_outcome: LcmAuthorityOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<LcmDoctorHealthV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmContentRangeV1 {
    pub offset: u64,
    pub limit: u64,
    pub returned_chars: u64,
    pub total_chars: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmStorageKindV1 {
    Inline,
    External,
    CanonicalOccurrence,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmMessageV1 {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: Option<i64>,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content: String,
    pub content_range: LcmContentRangeV1,
    pub content_hash: Option<String>,
    pub storage_kind: LcmStorageKindV1,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmTemporalFieldsV1 {
    pub anchors: Vec<String>,
    pub watermarks: TemporalWatermarksV1,
    pub authorized_root: Option<String>,
    pub coverage: TemporalCoverageV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<super::SessionSourceCoverageV1>,
    pub explanations: Vec<TemporalExplanationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<TemporalOmissionV1>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LcmRetrievalOutcomeV1 {
    Complete {
        freshness: super::TemporalFreshnessV1,
    },
    Partial {
        freshness: super::TemporalFreshnessV1,
        omitted: u64,
    },
    Stale {
        freshness: super::TemporalFreshnessV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmLoadSessionResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub messages: Vec<LcmMessageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_limit_clamped_from: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LcmTemporalFieldsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<super::RetrievalWorkerStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_sessions: Option<BTreeMap<String, usize>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LcmGrepHitV1 {
    pub kind: String,
    pub provider: String,
    pub session_id: String,
    pub message_id: Option<String>,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    pub role: Option<String>,
    pub snippet: String,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LcmGrepResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub hits: Vec<LcmGrepHitV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_sessions: Option<BTreeMap<String, usize>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LcmTemporalFieldsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<super::RetrievalWorkerStatusV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmSourceRefV1 {
    RawMessage { store_id: i64 },
    SummaryNode { node_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmSummaryNodeV1 {
    pub node_id: String,
    pub provider: String,
    pub conversation_id: String,
    pub session_id: String,
    pub depth: i64,
    pub summary_text: String,
    pub summary_hash: String,
    pub source_refs: Vec<LcmSourceRefV1>,
    pub summary_token_count: i64,
    pub source_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmRawMessageV1 {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content: String,
    pub content_hash: String,
    pub storage_kind: LcmStorageKindV1,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmRawMessageMetadataV1 {
    pub provider: String,
    pub message_id: String,
    pub session_id: String,
    pub store_id: i64,
    pub role: String,
    pub ordinal: i64,
    pub timestamp: Option<i64>,
    pub content_hash: String,
    pub storage_kind: LcmStorageKindV1,
    pub payload_ref: Option<String>,
    pub legacy_source: bool,
    pub legacy_truncated: bool,
    pub metadata_json: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmSourcePaginationV1 {
    pub source_limit: usize,
    pub returned_sources: usize,
    pub total_sources: usize,
    pub has_more: bool,
    pub remaining_sources: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandedSourceV1 {
    pub source_ref: LcmSourceRefV1,
    pub state: HydrationStateResultV1,
    pub content: String,
    pub content_range: Option<LcmContentRangeV1>,
    pub content_truncated: bool,
    pub raw_message: Option<LcmRawMessageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message_metadata: Option<LcmRawMessageMetadataV1>,
    pub summary_node: Option<Box<LcmSummaryNodeV1>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpansionV1 {
    pub kind: String,
    pub content: String,
    pub content_range: LcmContentRangeV1,
    pub raw_message: Option<LcmRawMessageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message_metadata: Option<LcmRawMessageMetadataV1>,
    pub summary_node: Option<LcmSummaryNodeV1>,
    pub summary_sources: Vec<LcmExpandedSourceV1>,
    pub payload_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_current_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub externalized_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pagination: Option<LcmSourcePaginationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmRawMessageOverviewV1 {
    pub message_id: String,
    pub store_id: i64,
    pub role: String,
    pub storage_kind: LcmStorageKindV1,
    pub payload_ref: Option<String>,
    pub content_preview: String,
    pub content_range: LcmContentRangeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmSummaryNodeOverviewV1 {
    pub node_id: String,
    pub conversation_id: String,
    pub depth: i64,
    pub summary_preview: String,
    pub source_count: usize,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescribeSourceOverviewV1 {
    pub source_kind: String,
    pub source_ref: LcmSourceRefV1,
    pub store_id: Option<i64>,
    pub node_id: Option<String>,
    pub role: Option<String>,
    pub storage_kind: Option<LcmStorageKindV1>,
    pub summary_token_count: Option<i64>,
    pub source_token_count: Option<i64>,
    pub expand_hint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescribeSummaryNodeV1 {
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
    pub children: Vec<LcmDescribeSourceOverviewV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescribeExternalPayloadV1 {
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescriptionV1 {
    pub target: String,
    pub provider: String,
    pub session_id: String,
    pub raw_message_count: i64,
    pub summary_node_count: i64,
    pub external_payload_count: i64,
    pub first_store_id: Option<i64>,
    pub last_store_id: Option<i64>,
    pub raw_messages: Vec<LcmRawMessageOverviewV1>,
    pub summary_nodes: Vec<LcmSummaryNodeOverviewV1>,
    pub summary_node: Option<LcmDescribeSummaryNodeV1>,
    pub external_payload: Option<LcmDescribeExternalPayloadV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token_estimate: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactLineageEdgeV1 {
    pub kind: String,
    pub subject_anchor_id: String,
    pub object_anchor_id: String,
    pub knowledge_at: i64,
    pub authority: String,
    pub authorized: bool,
    pub supporting_anchor_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescribeResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub description: Option<LcmDescriptionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<HydrationStateResultV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Vec<CompactLineageEdgeV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<LcmRetrievalOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LcmTemporalFieldsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<super::RetrievalWorkerStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_sessions: Option<BTreeMap<String, usize>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub expansion: Option<LcmExpansionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<HydrationStateResultV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<LcmRetrievalOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LcmTemporalFieldsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<super::RetrievalWorkerStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_sessions: Option<BTreeMap<String, usize>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQuerySynthesisPromptV1 {
    pub system: String,
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_prompt_truncated_for_mcp: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryBudgetV1 {
    pub requested_max_chars: usize,
    pub used_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryPaginationV1 {
    pub kind: String,
    pub node_id: Option<String>,
    pub source_ref: Option<LcmSourceRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<HydrationStateResultV1>,
    pub next_content_offset: Option<u64>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryMatchV1 {
    pub kind: String,
    pub node_id: Option<String>,
    pub store_id: Option<i64>,
    pub snippet: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryContextBlockV1 {
    pub kind: String,
    pub node_id: Option<String>,
    pub source_ref: Option<LcmSourceRefV1>,
    pub content: String,
    pub content_range: LcmContentRangeV1,
    pub raw_message: Option<LcmRawMessageV1>,
    pub summary_node: Option<LcmSummaryNodeV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub context_blocks: Vec<LcmExpandQueryContextBlockV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_synthesis: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis_prompt: Option<LcmExpandQuerySynthesisPromptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_max_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<LcmExpandQueryBudgetV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pagination: Option<Vec<LcmExpandQueryPaginationV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<LcmExpandQueryMatchV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<LcmTemporalFieldsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_response_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_truncation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_truncated_for_mcp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_truncated_for_mcp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RetainedErrorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<super::RetrievalWorkerStatusV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_sessions: Option<BTreeMap<String, usize>>,
}
