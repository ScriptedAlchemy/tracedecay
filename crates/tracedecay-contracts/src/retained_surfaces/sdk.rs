//! Canonical executable request models for retained memory and temporal reads.
//!
//! These are the one public wire authority used by the SDK registry and the
//! daemon-owned retained-surface service.  The old MCP handlers may still
//! render Markdown, but they must not grow a competing request DTO.

mod automation;
mod fact_store;
mod results;

pub use crate::memory::{FactCategoryV1, FactMetadataV1};
pub use automation::{
    AutomationRunRequestV1, AutomationTaskRequestV1, AutomationTaskV1, CombinedReviewRunInputV1,
    DEFAULT_FACT_STORE_CURATE_MIN_CONFIDENCE_MILLIONTHS, DEFAULT_FACT_STORE_CURATE_REVIEW_LIMIT,
    FactStoreCurateRequestV1, MemoryCuratorRunInputV1, SessionReflectorRunInputV1,
    SkillWriterRunInputV1, UserJobRunInputV1,
};
pub use fact_store::{
    FactSourceLabelPatchV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreGetRequestV1, FactStoreListRequestV1, FactStoreProbeRequestV1,
    FactStoreReasonRequestV1, FactStoreRelatedRequestV1, FactStoreRemoveRequestV1,
    FactStoreSearchRequestV1, FactStoreSupersedeRequestV1, FactStoreUpdateRequestV1,
};
pub use results::{
    AutomationCommittedReceiptV1, AutomationExternalEffectReceiptV1, AutomationRunProblemV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    ClosedUtcIntervalV1, CompactLineageEdgeV1, CorrelationIndexCountModeV1, CorrelationIndexV1,
    FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1, FactContradictionV1,
    FactFeedbackDetailsAvailabilityV1, FactFeedbackResultV1, FactFeedbackV1,
    FactIdentitySourceResultV1, FactPayloadAccessV1, FactProjectionV1,
    FactRetrievalTelemetryDegradationV1, FactRetrievalTelemetryV1, FactSearchCursorV1,
    FactSearchGraphCoverageV1, FactSearchGraphDegradationV1, FactSearchHitV1, FactSearchScoresV1,
    FactStatusV1, FactStoreAddCommitV1, FactStoreAddResultV1, FactStoreContradictResultV1,
    FactStoreGetResultV1, FactStoreListResultV1, FactStoreProbeResultV1, FactStoreReasonResultV1,
    FactStoreRelatedResultV1, FactStoreRemoveResultV1, FactStoreSearchResultV1,
    FactStoreSupersedeResultV1, FactStoreUpdateResultV1, FactTelemetryV1, FactV1, GitScopeV1,
    HydrationStateResultV1, LcmAuthorityOutcomeV1, LcmConfigStatusV1, LcmContentRangeV1,
    LcmDagDepthStatusV1, LcmDagStatusV1, LcmDescribeExternalPayloadV1, LcmDescribeResultV1,
    LcmDescribeSourceOverviewV1, LcmDescribeSummaryNodeV1, LcmDescriptionV1,
    LcmDoctorFindingKindV1, LcmDoctorFindingV1, LcmDoctorHealthStatusV1, LcmDoctorHealthV1,
    LcmDoctorProjectionStateV1, LcmDoctorProjectionV1, LcmDoctorResultV1, LcmExpandQueryBudgetV1,
    LcmExpandQueryContextBlockV1, LcmExpandQueryMatchV1, LcmExpandQueryPaginationV1,
    LcmExpandQueryResultV1, LcmExpandQuerySynthesisPromptV1, LcmExpandResultV1,
    LcmExpandedSourceV1, LcmExpansionV1, LcmGrepHitV1, LcmGrepResultV1, LcmLifecycleStatusV1,
    LcmLoadSessionResultV1, LcmMessageV1, LcmPayloadCoverageStateV1, LcmPayloadCoverageV1,
    LcmPayloadGcStatusV1, LcmPayloadStatusV1, LcmRawMessageMetadataV1, LcmRawMessageOverviewV1,
    LcmRawMessageV1, LcmRedactionStatusV1, LcmRetrievalOutcomeV1, LcmSourcePaginationV1,
    LcmSourceRefV1, LcmStatusResultV1, LcmStatusV1, LcmStorageKindV1, LcmStoreStatusV1,
    LcmStoreTokenCoverageV1, LcmSummaryNodeOverviewV1, LcmSummaryNodeV1, LcmTemporalFieldsV1,
    MemoryAlgebraV1, MemoryAutomationCurationAddDispositionV1,
    MemoryAutomationCurationLinkDispositionV1, MemoryAutomationCurationMergeV1,
    MemoryAutomationCurationOperationEffectV1, MemoryAutomationCurationReceiptV1,
    MemoryAutomationCurationRelationKindV1, MemoryAutomationCurationRelationProvenanceV1,
    MemoryAutomationCurationRelationV1, MemoryAutomationCurationRemoveDispositionV1,
    MemoryAutomationCurationResultV1, MemoryAutomationFactConflictSourceV1,
    MemoryAutomationFactConflictValidationV1, MemoryAutomationFactDedupeValidationV1,
    MemoryAutomationFactDispositionV1, MemoryAutomationFactEffectV1,
    MemoryAutomationFactEvidenceItemV1, MemoryAutomationFactEvidenceSourceSpanV1,
    MemoryAutomationFactEvidenceTrustBucketV1, MemoryAutomationFactEvidenceTrustV1,
    MemoryAutomationFactEvidenceV1, MemoryAutomationFactInputDigestError,
    MemoryAutomationFactInputDigestV1, MemoryAutomationFactNearestMatchV1,
    MemoryAutomationFactReceiptV1, MemoryAutomationFactRequestV1, MemoryAutomationFactStateV1,
    MemoryAutomationFactTargetV1, MemoryAutomationFactValidationStatusV1,
    MemoryAutomationFactValidationV1, MemoryFeedbackFunnelV1, MemoryStatusResultV1, MemoryStatusV1,
    MessageSearchHitV1, MessageSearchResultV1, RetainedErrorV1, RetainedNextActionV1,
    RetainedOutcomeStatusV1, RetainedSurfaceResultV1, RetrievalWorkerStatusV1,
    SessionCorrelationHitV1, SessionCoverageIntervalV1, SessionCoverageReasonV1,
    SessionCoverageRequestV1, SessionCoverageStateV1, SessionMessageV1, SessionRecordV1,
    SessionRefreshBeginResultV1, SessionRefreshCancelResultV1, SessionRefreshFrontierResultV1,
    SessionRefreshProgressV1, SessionRefreshReceiptV1, SessionRefreshStatusResultV1,
    SessionRefreshTerminalStateResultV1, SessionSourceCoverageV1, SessionsForResultV1,
    TemporalCoverageOmissionV1, TemporalCoverageV1, TemporalExplanationV1, TemporalFreshnessV1,
    TemporalMetadataV1, TemporalOmissionV1, TemporalPopulationCountV1, TemporalWatermarksV1,
    TrustHistoryEntryV1, ValidCoverageIntervalV1, WorkflowAgentV1, WorkflowCoverageV1,
    WorkflowQueryModeV1, WorkflowRunV1, WorkflowStatusV1, WorkflowsResultV1,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{FactEventId, FactId, ProjectId, TemporalModeV1};

use super::RetainedSurfaceOperation;

/// Exact registered-project selector shared by retained reads.
///
/// Inlined so every request schema advertises the closed selector contract
/// (`required: ["project_id"]`, no additional properties) directly on its
/// `project_selector` property instead of behind a `$defs` reference, matching
/// the selector shape the MCP dispatch policy promises for selector-accepting
/// tools.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub struct RetainedProjectSelectorV1 {
    pub project_id: ProjectId,
}

/// The temporal filter intentionally retains the established integer-or-text
/// wire form (Unix timestamps, RFC3339, and relative expressions).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum RetainedTimeFilterV1 {
    Micros(u64),
    Expression(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScopeV1 {
    Project,
    User,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactReadOptionsV1 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<FactCategoryV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_trust: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactFeedbackActionV1 {
    Helpful,
    Unhelpful,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactFeedbackRequestV1 {
    pub fact_id: FactId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_last_event_id: Option<FactEventId>,
    pub action: FactFeedbackActionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStatusRequestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRelationshipScopeV1 {
    All,
    ParentsOnly,
    SubagentsOnly,
}

impl MessageRelationshipScopeV1 {
    /// Advertised order for MCP schemas and the CLI parser. Labels are the
    /// serde `snake_case` names.
    pub const WIRE: [&'static str; 3] = [
        Self::All.as_str(),
        Self::ParentsOnly.as_str(),
        Self::SubagentsOnly.as_str(),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ParentsOnly => "parents_only",
            Self::SubagentsOnly => "subagents_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "parents_only" => Some(Self::ParentsOnly),
            "subagents_only" => Some(Self::SubagentsOnly),
            _ => None,
        }
    }
}

#[cfg(test)]
mod relationship_scope_wire_tests {
    use serde_json::json;

    use super::MessageRelationshipScopeV1;

    #[test]
    fn scope_labels_match_serde() {
        let scopes = [
            MessageRelationshipScopeV1::All,
            MessageRelationshipScopeV1::ParentsOnly,
            MessageRelationshipScopeV1::SubagentsOnly,
        ];
        let labels: Vec<&str> = scopes
            .iter()
            .copied()
            .map(MessageRelationshipScopeV1::as_str)
            .collect();
        assert_eq!(labels.as_slice(), MessageRelationshipScopeV1::WIRE);
        for scope in scopes {
            assert_eq!(
                serde_json::to_value(scope).expect("scope serializes"),
                json!(scope.as_str())
            );
            assert_eq!(
                MessageRelationshipScopeV1::parse(scope.as_str()),
                Some(scope)
            );
        }
        assert_eq!(MessageRelationshipScopeV1::parse(" all"), None);
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageTypeFilterV1 {
    All,
    DirectUser,
    ToolResult,
}

impl MessageTypeFilterV1 {
    /// Advertised order for MCP schemas and the CLI parser. Labels are the
    /// serde `snake_case` names.
    pub const WIRE: [&'static str; 3] = [
        Self::All.as_str(),
        Self::DirectUser.as_str(),
        Self::ToolResult.as_str(),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::DirectUser => "direct_user",
            Self::ToolResult => "tool_result",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "direct_user" => Some(Self::DirectUser),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }
}

#[cfg(test)]
mod message_type_wire_tests {
    use serde_json::json;

    use super::MessageTypeFilterV1;

    #[test]
    fn message_type_labels_match_serde() {
        let types = [
            MessageTypeFilterV1::All,
            MessageTypeFilterV1::DirectUser,
            MessageTypeFilterV1::ToolResult,
        ];
        let labels: Vec<&str> = types
            .iter()
            .copied()
            .map(MessageTypeFilterV1::as_str)
            .collect();
        assert_eq!(labels.as_slice(), MessageTypeFilterV1::WIRE);
        for message_type in types {
            assert_eq!(
                serde_json::to_value(message_type).expect("message type serializes"),
                json!(message_type.as_str())
            );
            assert_eq!(
                MessageTypeFilterV1::parse(message_type.as_str()),
                Some(message_type)
            );
        }
        assert_eq!(MessageTypeFilterV1::parse("tool"), None);
    }
}

/// Exact public input accepted by `tracedecay_message_search`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchRequestV1 {
    pub query: Option<String>,
    #[serde(default)]
    pub goals: bool,
    pub provider: Option<String>,
    pub project_key: Option<String>,
    pub include_subagents: Option<bool>,
    /// Freshness precondition: stale or partial coverage returns
    /// `refresh_required` instead of stored evidence. The read never refreshes.
    pub require_fresh: Option<bool>,
    pub cursor: Option<String>,
    pub parent_session_id: Option<String>,
    pub since: Option<RetainedTimeFilterV1>,
    pub until: Option<RetainedTimeFilterV1>,
    pub scope: Option<MessageRelationshipScopeV1>,
    pub message_type: Option<MessageTypeFilterV1>,
    pub limit: Option<u64>,
    pub project_selector: Option<RetainedProjectSelectorV1>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
    pub workflow_run: Option<String>,
    pub workflow_agent: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionGitRefV1 {
    Branch,
    Worktree,
    Commit,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionGitRelationV1 {
    Produced,
    Observed,
    All,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionsForRequestV1 {
    pub git_ref: SessionGitRefV1,
    pub value: String,
    pub since: Option<RetainedTimeFilterV1>,
    pub until: Option<RetainedTimeFilterV1>,
    pub relation: Option<SessionGitRelationV1>,
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsRequestV1 {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub agent_label: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
    pub limit: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmStatusRequestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<bool>,
}

#[cfg(test)]
mod lcm_status_request_tests {
    use serde_json::json;

    use super::LcmStatusRequestV1;

    #[test]
    fn status_request_omits_unspecified_optional_fields_for_the_mounted_handler() {
        let request = LcmStatusRequestV1 {
            provider: None,
            session_id: Some("stock-check-session".to_owned()),
            deep: None,
        };

        assert_eq!(
            serde_json::to_value(request).expect("status request serializes"),
            json!({"session_id": "stock-check-session"})
        );
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDoctorRequestV1 {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmLoadSessionRequestV1 {
    pub provider: Option<String>,
    pub session_id: String,
    pub cursor: Option<String>,
    pub temporal_mode: Option<TemporalModeV1>,
    pub limit: Option<u64>,
    pub role: Option<String>,
    pub roles: Option<Vec<String>>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub content_offset: Option<u64>,
    pub content_limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmSearchScopeV1 {
    Current,
    Session,
    All,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmGrepSortV1 {
    Recency,
    Relevance,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmRoleV1 {
    System,
    User,
    Assistant,
    Tool,
    Unknown,
}

impl LcmRoleV1 {
    /// Advertised order for MCP schemas. Labels are [`Self::as_str`], which is
    /// the serde `snake_case` name, so the catalog cannot drift from the wire.
    pub const WIRE: [&'static str; 5] = [
        Self::System.as_str(),
        Self::User.as_str(),
        Self::Assistant.as_str(),
        Self::Tool.as_str(),
        Self::Unknown.as_str(),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[cfg(test)]
mod lcm_role_wire_tests {
    use serde_json::json;

    use super::LcmRoleV1;

    #[test]
    fn role_labels_match_serde_and_reject_host_aliases() {
        let roles = [
            LcmRoleV1::System,
            LcmRoleV1::User,
            LcmRoleV1::Assistant,
            LcmRoleV1::Tool,
            LcmRoleV1::Unknown,
        ];
        let labels: Vec<&str> = roles.iter().copied().map(LcmRoleV1::as_str).collect();
        assert_eq!(labels.as_slice(), LcmRoleV1::WIRE);
        for role in roles {
            assert_eq!(
                serde_json::to_value(role).expect("role serializes"),
                json!(role.as_str())
            );
            assert_eq!(LcmRoleV1::parse(role.as_str()), Some(role));
        }
        assert_eq!(LcmRoleV1::parse("developer"), None);
        assert_eq!(LcmRoleV1::parse(" model"), None);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmGrepRequestV1 {
    pub provider: Option<String>,
    pub query: String,
    pub scope: Option<LcmSearchScopeV1>,
    pub relationship_scope: Option<MessageRelationshipScopeV1>,
    pub message_type: Option<MessageTypeFilterV1>,
    pub session_id: Option<String>,
    pub include_summaries: Option<bool>,
    pub sort: Option<LcmGrepSortV1>,
    pub source: Option<String>,
    pub role: Option<LcmRoleV1>,
    pub start_time: Option<RetainedTimeFilterV1>,
    pub end_time: Option<RetainedTimeFilterV1>,
    pub since: Option<RetainedTimeFilterV1>,
    pub until: Option<RetainedTimeFilterV1>,
    pub limit: Option<u64>,
    pub cursor: Option<String>,
    pub temporal_mode: Option<TemporalModeV1>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmDescribeTargetV1 {
    Session,
    SummaryNode { node_id: String },
    ExternalPayload { payload_ref: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmDescribeRequestV1 {
    pub provider: String,
    pub session_id: String,
    pub target: Option<LcmDescribeTargetV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmExpandTargetV1 {
    CanonicalOccurrence { message_id: String },
    RawMessage { store_id: u64 },
    SummaryNode { node_id: String },
    ExternalPayload { payload_ref: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandRequestV1 {
    pub provider: String,
    pub session_id: String,
    pub target: LcmExpandTargetV1,
    pub content_offset: Option<u64>,
    pub content_limit: Option<u64>,
    pub source_limit: Option<u64>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum LcmNodeIdV1 {
    Text(String),
    Numeric(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmExpandQueryRequestV1 {
    pub provider: String,
    pub session_id: String,
    pub query: Option<String>,
    pub prompt: String,
    pub node_ids: Option<Vec<LcmNodeIdV1>>,
    pub max_results: Option<u64>,
    pub max_tokens: Option<u64>,
    pub context_max_tokens: Option<u64>,
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRefreshActionV1 {
    Status,
    Cancel,
    Begin,
}

/// Session-store owner one refresh is bound to.
///
/// The caller selects the already-mounted project or profile authority. Exact
/// profile, project, repository, worktree, branch, store, and root identities
/// stay daemon-owned and are resolved from that authority during admission.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionRefreshScopeV1 {
    Project {},
    Profile {},
}

impl SessionRefreshScopeV1 {
    /// Wire spelling of the selected owner, echoed in refresh results.
    #[hotpath::skip]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Project {} => "project",
            Self::Profile {} => "profile",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSessionV1 {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSourceV1 {
    pub scope: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRefreshGrainV1 {
    Occurrence,
    LogicalMessage,
    Turn,
    Session,
    Thread,
    Agent,
    Summary,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshFrontierV1 {
    pub observed_through: u64,
    pub committed_through: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshTargetV1 {
    pub temporal_mode: TemporalModeV1,
    pub grain: SessionRefreshGrainV1,
    pub frontier: SessionRefreshFrontierV1,
}

/// Exact route-selected session-refresh request body.
///
/// Each current route selects the action itself; `scope` selects the mounted
/// session-store owner. Project-scoped requests are served under project-open
/// admission, profile-scoped requests by the authenticated profile authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshActionRequestV1 {
    pub scope: SessionRefreshScopeV1,
    pub session: SessionRefreshSessionV1,
    pub source: SessionRefreshSourceV1,
    pub target: SessionRefreshTargetV1,
    pub handle: Option<String>,
}

/// Operation-selected request used by the canonical application owner.
/// Current HTTP bindings deserialize [`SessionRefreshActionRequestV1`] and
/// attach one of the three mounted actions before dispatch.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshRequestV1 {
    pub action: SessionRefreshActionV1,
    #[serde(flatten)]
    pub request: SessionRefreshActionRequestV1,
}

impl SessionRefreshRequestV1 {
    #[hotpath::skip]
    pub const fn with_action(
        action: SessionRefreshActionV1,
        request: SessionRefreshActionRequestV1,
    ) -> Self {
        Self { action, request }
    }

    #[hotpath::skip]
    pub const fn operation(&self) -> RetainedSurfaceOperation {
        match self.action {
            SessionRefreshActionV1::Status => RetainedSurfaceOperation::SessionRefreshStatus,
            SessionRefreshActionV1::Cancel => RetainedSurfaceOperation::SessionRefreshCancel,
            SessionRefreshActionV1::Begin => RetainedSurfaceOperation::SessionRefreshBegin,
        }
    }
}

#[cfg(test)]
mod session_refresh_request_tests {
    use serde_json::json;

    use super::{SessionRefreshActionRequestV1, SessionRefreshRequestV1, SessionRefreshScopeV1};

    fn route_body() -> serde_json::Value {
        json!({
            "scope": { "kind": "project" },
            "session": { "id": "session.1" },
            "source": { "scope": "cursor" },
            "target": {
                "temporal_mode": { "kind": "current" },
                "grain": "session",
                "frontier": { "observed_through": 0, "committed_through": 0 }
            },
            "handle": null
        })
    }

    #[test]
    fn route_selected_refresh_request_rejects_an_action_tag() {
        let mut body = route_body();
        body["action"] = json!("status");
        assert!(serde_json::from_value::<SessionRefreshActionRequestV1>(body).is_err());
    }

    #[test]
    fn profile_scope_carries_no_daemon_owned_identity() {
        let mut body = route_body();
        body["scope"] = json!({ "kind": "profile" });
        let request = serde_json::from_value::<SessionRefreshActionRequestV1>(body)
            .expect("profile-scoped request");
        assert_eq!(request.scope, SessionRefreshScopeV1::Profile {});
        assert_eq!(request.scope.as_str(), "profile");
    }

    #[test]
    fn scope_rejects_untyped_and_internal_owner_selectors() {
        for scope in [
            json!("profile"),
            json!({ "kind": "profile", "profile_id": "profile.default" }),
            json!({ "kind": "project", "project": {} }),
            json!({ "kind": "user", "profile_id": "profile.default" }),
        ] {
            let mut body = route_body();
            body["scope"] = scope.clone();
            assert!(
                serde_json::from_value::<SessionRefreshActionRequestV1>(body).is_err(),
                "scope {scope} must be refused"
            );
        }
        let mut body = route_body();
        body["profile"] = json!({ "id": "profile.default" });
        assert!(serde_json::from_value::<SessionRefreshActionRequestV1>(body).is_err());
    }

    #[test]
    fn current_refresh_request_rejects_legacy_action_aliases() {
        for action in ["start", "join", "resume"] {
            let mut body = route_body();
            body["action"] = json!(action);
            assert!(serde_json::from_value::<SessionRefreshRequestV1>(body).is_err());
        }
    }

    #[test]
    fn application_owner_attaches_the_canonical_action_tag() {
        let mut body = route_body();
        body["action"] = json!("status");
        let request = serde_json::from_value::<SessionRefreshRequestV1>(body)
            .expect("canonical operation-selected request");
        assert!(matches!(
            request.action,
            super::SessionRefreshActionV1::Status
        ));
    }

    #[test]
    fn application_owner_accepts_an_as_of_cutoff() {
        let mut body = route_body();
        body["action"] = json!("status");
        body["target"]["temporal_mode"] = json!({ "kind": "as_of", "cutoff": 42 });
        let request = serde_json::from_value::<SessionRefreshRequestV1>(body)
            .expect("canonical as-of request");
        assert_eq!(
            request.request.target.temporal_mode,
            tracedecay_domain::TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(42)
            }
        );
    }
}

/// Operation-tagged request accepted by the daemon-owned retained-surface
/// service. The tag is internal to the canonical route owner; HTTP and MCP
/// select the operation from their binding and deserialize the matching inner
/// request directly.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(
    deny_unknown_fields,
    tag = "operation",
    content = "request",
    rename_all = "snake_case"
)]
pub enum RetainedSurfaceRequestV1 {
    FactStoreCurate(FactStoreCurateRequestV1),
    FactStoreAdd(FactStoreAddRequestV1),
    FactStoreSearch(FactStoreSearchRequestV1),
    FactStoreProbe(FactStoreProbeRequestV1),
    FactStoreRelated(FactStoreRelatedRequestV1),
    FactStoreReason(FactStoreReasonRequestV1),
    FactStoreContradict(FactStoreContradictRequestV1),
    FactStoreGet(FactStoreGetRequestV1),
    FactStoreUpdate(FactStoreUpdateRequestV1),
    FactStoreRemove(FactStoreRemoveRequestV1),
    FactStoreSupersede(FactStoreSupersedeRequestV1),
    FactStoreList(FactStoreListRequestV1),
    FactFeedback(FactFeedbackRequestV1),
    MemoryStatus(MemoryStatusRequestV1),
    SessionRefresh(SessionRefreshRequestV1),
    MessageSearch(MessageSearchRequestV1),
    SessionsFor(SessionsForRequestV1),
    Workflows(WorkflowsRequestV1),
    LcmStatus(LcmStatusRequestV1),
    LcmDoctor(LcmDoctorRequestV1),
    LcmLoadSession(LcmLoadSessionRequestV1),
    LcmGrep(LcmGrepRequestV1),
    LcmDescribe(LcmDescribeRequestV1),
    LcmExpand(LcmExpandRequestV1),
    LcmExpandQuery(LcmExpandQueryRequestV1),
}

impl RetainedSurfaceRequestV1 {
    #[hotpath::skip]
    pub const fn operation(&self) -> RetainedSurfaceOperation {
        match self {
            Self::FactStoreCurate(_) => RetainedSurfaceOperation::FactStoreCurate,
            Self::FactStoreAdd(_) => RetainedSurfaceOperation::FactStoreAdd,
            Self::FactStoreSearch(_) => RetainedSurfaceOperation::FactStoreSearch,
            Self::FactStoreProbe(_) => RetainedSurfaceOperation::FactStoreProbe,
            Self::FactStoreRelated(_) => RetainedSurfaceOperation::FactStoreRelated,
            Self::FactStoreReason(_) => RetainedSurfaceOperation::FactStoreReason,
            Self::FactStoreContradict(_) => RetainedSurfaceOperation::FactStoreContradict,
            Self::FactStoreGet(_) => RetainedSurfaceOperation::FactStoreGet,
            Self::FactStoreUpdate(_) => RetainedSurfaceOperation::FactStoreUpdate,
            Self::FactStoreRemove(_) => RetainedSurfaceOperation::FactStoreRemove,
            Self::FactStoreSupersede(_) => RetainedSurfaceOperation::FactStoreSupersede,
            Self::FactStoreList(_) => RetainedSurfaceOperation::FactStoreList,
            Self::FactFeedback(_) => RetainedSurfaceOperation::FactFeedback,
            Self::MemoryStatus(_) => RetainedSurfaceOperation::MemoryStatus,
            Self::SessionRefresh(request) => request.operation(),
            Self::MessageSearch(_) => RetainedSurfaceOperation::MessageSearch,
            Self::SessionsFor(_) => RetainedSurfaceOperation::SessionsFor,
            Self::Workflows(_) => RetainedSurfaceOperation::Workflows,
            Self::LcmStatus(_) => RetainedSurfaceOperation::LcmStatus,
            Self::LcmDoctor(_) => RetainedSurfaceOperation::LcmDoctor,
            Self::LcmLoadSession(_) => RetainedSurfaceOperation::LcmLoadSession,
            Self::LcmGrep(_) => RetainedSurfaceOperation::LcmGrep,
            Self::LcmDescribe(_) => RetainedSurfaceOperation::LcmDescribe,
            Self::LcmExpand(_) => RetainedSurfaceOperation::LcmExpand,
            Self::LcmExpandQuery(_) => RetainedSurfaceOperation::LcmExpandQuery,
        }
    }
}
