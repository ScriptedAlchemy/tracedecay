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
    FactStoreSearchRequestV1, FactStoreUpdateRequestV1,
};
pub use results::{
    AutomationCommittedReceiptV1, AutomationExternalEffectReceiptV1, AutomationRunProblemV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    ClosedUtcIntervalV1, CompactLineageEdgeV1, CorrelationIndexV1, FactCommitDispositionV1,
    FactCommitOwnerV1, FactCommitReceiptV1, FactContradictionV1, FactFeedbackDetailsAvailabilityV1,
    FactFeedbackResultV1, FactFeedbackV1, FactIdentitySourceResultV1, FactPayloadAccessV1,
    FactProjectionV1, FactSearchCursorV1, FactSearchGraphCoverageV1, FactSearchGraphDegradationV1,
    FactSearchHitV1, FactSearchScoresV1, FactStatusV1, FactStoreAddCommitV1, FactStoreAddResultV1,
    FactStoreContradictResultV1, FactStoreGetResultV1, FactStoreListResultV1,
    FactStoreProbeResultV1, FactStoreReasonResultV1, FactStoreRelatedResultV1,
    FactStoreRemoveResultV1, FactStoreSearchResultV1, FactStoreUpdateResultV1, FactTelemetryV1,
    FactV1, GitScopeV1, HydrationStateResultV1, LcmAuthorityOutcomeV1, LcmConfigStatusV1,
    LcmContentRangeV1, LcmDagDepthStatusV1, LcmDagStatusV1, LcmDescribeExternalPayloadV1,
    LcmDescribeResultV1, LcmDescribeSourceOverviewV1, LcmDescribeSummaryNodeV1, LcmDescriptionV1,
    LcmDoctorFindingKindV1, LcmDoctorFindingV1, LcmDoctorHealthStatusV1, LcmDoctorHealthV1,
    LcmDoctorResultV1, LcmExpandQueryBudgetV1, LcmExpandQueryContextBlockV1, LcmExpandQueryMatchV1,
    LcmExpandQueryPaginationV1, LcmExpandQueryResultV1, LcmExpandQuerySynthesisPromptV1,
    LcmExpandResultV1, LcmExpandedSourceV1, LcmExpansionV1, LcmGrepHitV1, LcmGrepResultV1,
    LcmLifecycleStatusV1, LcmLoadSessionResultV1, LcmMessageV1, LcmPayloadCoverageStateV1,
    LcmPayloadCoverageV1, LcmPayloadGcStatusV1, LcmPayloadStatusV1, LcmRawMessageMetadataV1,
    LcmRawMessageOverviewV1, LcmRawMessageV1, LcmRedactionStatusV1, LcmRetrievalOutcomeV1,
    LcmSourcePaginationV1, LcmSourceRefV1, LcmStatusResultV1, LcmStatusV1, LcmStorageKindV1,
    LcmStoreStatusV1, LcmStoreTokenCoverageV1, LcmSummaryNodeOverviewV1, LcmSummaryNodeV1,
    LcmTemporalFieldsV1, MemoryAlgebraV1, MemoryAutomationCurationAddDispositionV1,
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
    MessageSearchFreshnessV1, MessageSearchHitV1, MessageSearchResultV1, MessageSearchRootV1,
    MessageSearchSkipV1, RetainedErrorV1, RetainedNextActionV1, RetainedOutcomeStatusV1,
    RetainedSurfaceResultV1, RetrievalWorkerStatusV1, SessionCorrelationHitV1,
    SessionCoverageIntervalV1, SessionCoverageModeV1, SessionCoverageReasonV1,
    SessionCoverageRequestV1, SessionCoverageStateV1, SessionMessageV1, SessionRecordV1,
    SessionRefreshBeginResultV1, SessionRefreshCancelResultV1, SessionRefreshFrontierResultV1,
    SessionRefreshProgressV1, SessionRefreshReceiptV1, SessionRefreshResultV1,
    SessionRefreshStatusResultV1, SessionRefreshTerminalStateResultV1, SessionSourceCoverageV1,
    SessionsForResultV1, TemporalCoverageV1, TemporalExplanationV1, TemporalFreshnessV1,
    TemporalMetadataV1, TemporalOmissionV1, TemporalWatermarksV1, TrustHistoryEntryV1,
    ValidCoverageIntervalV1, WorkflowAgentV1, WorkflowCoverageV1, WorkflowQueryModeV1,
    WorkflowRunV1, WorkflowStatusV1, WorkflowsResultV1,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{FactEventId, FactId, ProjectId};

use super::RetainedSurfaceOperation;

/// Output formatting accepted by legacy MCP calls. SDK and HTTP callers use
/// JSON, but accepting this field keeps the schema aligned with the mounted
/// MCP request form while the transport discards presentation-only controls.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetainedOutputFormatV1 {
    Markdown,
    Json,
}

/// Exact registered-project selector shared by retained reads.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageTypeFilterV1 {
    All,
    DirectUser,
    ToolResult,
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
    pub catch_up: Option<bool>,
    pub cursor: Option<String>,
    pub parent_session_id: Option<String>,
    pub since: Option<RetainedTimeFilterV1>,
    pub until: Option<RetainedTimeFilterV1>,
    pub time_from: Option<RetainedTimeFilterV1>,
    pub time_to: Option<RetainedTimeFilterV1>,
    pub scope: Option<MessageRelationshipScopeV1>,
    pub message_type: Option<MessageTypeFilterV1>,
    pub limit: Option<u64>,
    pub project_selector: Option<RetainedProjectSelectorV1>,
    pub project_id: Option<String>,
    pub project_path: Option<String>,
    pub project_scope: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
    pub workflow_run: Option<String>,
    pub workflow_agent: Option<String>,
    pub format: Option<RetainedOutputFormatV1>,
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
    pub format: Option<RetainedOutputFormatV1>,
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
    pub format: Option<RetainedOutputFormatV1>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
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
            format: None,
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LcmTemporalModeV1 {
    Current,
    AsOf,
    Evolution,
    Forensic,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LcmLoadSessionRequestV1 {
    pub provider: Option<String>,
    pub session_id: String,
    pub cursor: Option<String>,
    pub temporal_mode: Option<LcmTemporalModeV1>,
    pub as_of_micros: Option<u64>,
    pub limit: Option<u64>,
    pub role: Option<String>,
    pub roles: Option<Vec<String>>,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub content_offset: Option<u64>,
    pub content_limit: Option<u64>,
    pub format: Option<RetainedOutputFormatV1>,
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
    pub temporal_mode: Option<LcmTemporalModeV1>,
    pub as_of_micros: Option<u64>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub commit: Option<String>,
    pub format: Option<RetainedOutputFormatV1>,
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
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LcmExpandTargetV1 {
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
    pub format: Option<RetainedOutputFormatV1>,
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
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRefreshActionV1 {
    Status,
    Cancel,
    Begin,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshProjectV1 {
    pub id: String,
    pub profile_id: String,
    pub repository_id: String,
    pub worktree_id: String,
    pub branch_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSessionV1 {
    pub id: String,
    pub store_id: String,
    pub root_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSourceV1 {
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionRefreshTemporalModeV1 {
    Current,
    AsOf { cutoff: u64 },
    Evolution,
    Forensic,
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
    pub temporal_mode: SessionRefreshTemporalModeV1,
    pub grain: SessionRefreshGrainV1,
    pub frontier: SessionRefreshFrontierV1,
}

/// Exact route-selected session-refresh request body.
///
/// Each current route selects the action itself. Project identity is required
/// because these routes are mounted only under project-open admission.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshActionRequestV1 {
    pub project: SessionRefreshProjectV1,
    pub session: SessionRefreshSessionV1,
    pub source: SessionRefreshSourceV1,
    pub target: SessionRefreshTargetV1,
    pub handle: Option<String>,
    pub format: Option<RetainedOutputFormatV1>,
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
    pub const fn with_action(
        action: SessionRefreshActionV1,
        request: SessionRefreshActionRequestV1,
    ) -> Self {
        Self { action, request }
    }

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

    use super::{SessionRefreshActionRequestV1, SessionRefreshRequestV1};

    fn route_body() -> serde_json::Value {
        json!({
            "project": {
                "id": "project.1",
                "profile_id": "profile.default",
                "repository_id": "repository.1",
                "worktree_id": "worktree.1",
                "branch_id": "branch.1"
            },
            "session": {
                "id": "session.1",
                "store_id": "store.1",
                "root_id": "root.1"
            },
            "source": { "scope": "cursor" },
            "target": {
                "temporal_mode": { "kind": "current" },
                "grain": "session",
                "frontier": { "observed_through": 0, "committed_through": 0 }
            },
            "handle": null,
            "format": "json"
        })
    }

    #[test]
    fn route_selected_refresh_request_rejects_an_action_tag() {
        let mut body = route_body();
        body["action"] = json!("status");
        assert!(serde_json::from_value::<SessionRefreshActionRequestV1>(body).is_err());
    }

    #[test]
    fn current_refresh_request_rejects_legacy_scope_selection() {
        let mut body = route_body();
        body["scope"] = json!("profile");
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
        assert!(matches!(
            request.request.target.temporal_mode,
            super::SessionRefreshTemporalModeV1::AsOf { cutoff: 42 }
        ));
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
