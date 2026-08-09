//! Closed result authority for retained memory and temporal operations.

mod lcm;
mod memory;
mod session;

pub use lcm::{
    CompactLineageEdgeV1, LcmAuthorityOutcomeV1, LcmConfigStatusV1, LcmContentRangeV1,
    LcmDagDepthStatusV1, LcmDagStatusV1, LcmDescribeExternalPayloadV1, LcmDescribeResultV1,
    LcmDescribeSourceOverviewV1, LcmDescribeSummaryNodeV1, LcmDescriptionV1,
    LcmDoctorFindingKindV1, LcmDoctorFindingV1, LcmDoctorHealthStatusV1, LcmDoctorHealthV1,
    LcmDoctorResultV1, LcmExpandQueryBudgetV1, LcmExpandQueryContextBlockV1, LcmExpandQueryMatchV1,
    LcmExpandQueryPaginationV1, LcmExpandQueryResultV1, LcmExpandQuerySynthesisPromptV1,
    LcmExpandResultV1, LcmExpandedSourceV1, LcmExpansionV1, LcmGrepHitV1, LcmGrepResultV1,
    LcmLifecycleStatusV1, LcmLoadSessionResultV1, LcmMessageV1, LcmPayloadCoverageStateV1,
    LcmPayloadCoverageV1, LcmPayloadGcStatusV1, LcmPayloadStatusV1, LcmRawMessageMetadataV1,
    LcmRawMessageOverviewV1, LcmRawMessageV1, LcmRedactionStatusV1, LcmRetrievalOutcomeV1,
    LcmSourcePaginationV1, LcmSourceRefV1, LcmStatusResultV1, LcmStatusV1, LcmStorageKindV1,
    LcmStoreStatusV1, LcmStoreTokenCoverageV1, LcmSummaryNodeOverviewV1, LcmSummaryNodeV1,
    LcmTemporalFieldsV1,
};
pub use memory::{
    FactCollectionEntryV1, FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1,
    FactContradictionV1, FactDiffKindV1, FactFeedbackResultV1, FactFeedbackV1,
    FactMutationReceiptV1, FactSearchHitV1, FactStoreAddResultV1, FactStoreContradictResultV1,
    FactStoreGetResultV1, FactStoreListResultV1, FactStoreProbeResultV1, FactStoreReasonResultV1,
    FactStoreRelatedResultV1, FactStoreRemoveResultV1, FactStoreResultV1, FactStoreSearchResultV1,
    FactStoreUpdateResultV1, FactV1, MemoryFeedbackFunnelV1, MemoryRepairStatsV1,
    MemoryStatusResultV1, MemoryStatusV1, TrustHistoryEntryV1,
};
pub use session::{
    ClosedUtcIntervalV1, CorrelationIndexV1, GitScopeV1, HydrationStateResultV1,
    MessageSearchFreshnessV1, MessageSearchHitV1, MessageSearchResultV1, MessageSearchRootV1,
    MessageSearchSkipV1, RetainedNextActionV1, RetrievalWorkerStatusV1, SessionCorrelationHitV1,
    SessionCoverageIntervalV1, SessionCoverageModeV1, SessionCoverageReasonV1,
    SessionCoverageRequestV1, SessionCoverageStateV1, SessionMessageV1, SessionRecordV1,
    SessionRefreshBeginResultV1, SessionRefreshCancelResultV1, SessionRefreshFrontierResultV1,
    SessionRefreshProgressV1, SessionRefreshReceiptV1, SessionRefreshResultV1,
    SessionRefreshStatusResultV1, SessionRefreshTerminalStateResultV1, SessionSourceCoverageV1,
    SessionsForResultV1, TemporalCoverageV1, TemporalExplanationV1, TemporalFreshnessV1,
    TemporalMetadataV1, TemporalOmissionV1, TemporalWatermarksV1, ValidCoverageIntervalV1,
    WorkflowAgentV1, WorkflowCoverageV1, WorkflowQueryModeV1, WorkflowRunV1, WorkflowStatusV1,
    WorkflowsResultV1,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetainedOutcomeStatusV1 {
    Aborted,
    BudgetExhausted,
    Busy,
    Cancelled,
    Complete,
    CompleteZero,
    CursorManifestLimitExceeded,
    DeadlineExceeded,
    Deleted,
    Denied,
    Error,
    Failed,
    Joined,
    Locked,
    NotFound,
    Ok,
    Partial,
    Recorded,
    Redacted,
    Running,
    Stale,
    Started,
    Unavailable,
    UnsupportedFilter,
    WrongScope,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedErrorV1 {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum RetainedSurfaceResultV1 {
    FactStoreAdd(FactStoreAddResultV1),
    FactStoreSearch(FactStoreSearchResultV1),
    FactStoreProbe(FactStoreProbeResultV1),
    FactStoreRelated(FactStoreRelatedResultV1),
    FactStoreReason(FactStoreReasonResultV1),
    FactStoreContradict(FactStoreContradictResultV1),
    FactStoreGet(FactStoreGetResultV1),
    FactStoreUpdate(FactStoreUpdateResultV1),
    FactStoreRemove(FactStoreRemoveResultV1),
    FactStoreList(FactStoreListResultV1),
    FactFeedback(FactFeedbackResultV1),
    MemoryStatus(MemoryStatusResultV1),
    SessionRefreshStatus(SessionRefreshStatusResultV1),
    SessionRefreshCancel(SessionRefreshCancelResultV1),
    SessionRefreshBegin(SessionRefreshBeginResultV1),
    MessageSearch(MessageSearchResultV1),
    SessionsFor(SessionsForResultV1),
    Workflows(WorkflowsResultV1),
    LcmStatus(LcmStatusResultV1),
    LcmDoctor(LcmDoctorResultV1),
    LcmLoadSession(LcmLoadSessionResultV1),
    LcmGrep(LcmGrepResultV1),
    LcmDescribe(LcmDescribeResultV1),
    LcmExpand(LcmExpandResultV1),
    LcmExpandQuery(LcmExpandQueryResultV1),
}
