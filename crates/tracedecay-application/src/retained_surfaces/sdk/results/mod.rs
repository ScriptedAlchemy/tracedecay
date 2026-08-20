//! Closed result authority for retained memory and temporal operations.

mod automation;
mod lcm;
mod memory;
mod session;

pub use automation::{
    AutomationCommittedReceiptV1, AutomationExternalEffectReceiptV1, AutomationRunProblemV1,
    AutomationRunResultV1, AutomationRunSummaryV1, AutomationRunTerminalV1, AutomationSkipReasonV1,
    MemoryAutomationCurationAddDispositionV1, MemoryAutomationCurationLinkDispositionV1,
    MemoryAutomationCurationMergeV1, MemoryAutomationCurationOperationEffectV1,
    MemoryAutomationCurationReceiptV1, MemoryAutomationCurationRelationKindV1,
    MemoryAutomationCurationRelationProvenanceV1, MemoryAutomationCurationRelationV1,
    MemoryAutomationCurationRemoveDispositionV1, MemoryAutomationCurationResultV1,
    MemoryAutomationFactConflictSourceV1, MemoryAutomationFactConflictValidationV1,
    MemoryAutomationFactDedupeValidationV1, MemoryAutomationFactDispositionV1,
    MemoryAutomationFactEffectV1, MemoryAutomationFactEvidenceItemV1,
    MemoryAutomationFactEvidenceSourceSpanV1, MemoryAutomationFactEvidenceTrustBucketV1,
    MemoryAutomationFactEvidenceTrustV1, MemoryAutomationFactEvidenceV1,
    MemoryAutomationFactInputDigestError, MemoryAutomationFactInputDigestV1,
    MemoryAutomationFactNearestMatchV1, MemoryAutomationFactReceiptV1,
    MemoryAutomationFactRequestV1, MemoryAutomationFactStateV1, MemoryAutomationFactTargetV1,
    MemoryAutomationFactValidationStatusV1, MemoryAutomationFactValidationV1,
    SESSION_EVIDENCE_BUDGET_SUPPRESSED,
};
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
    FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1, FactContradictionV1,
    FactFeedbackDetailsAvailabilityV1, FactFeedbackResultV1, FactFeedbackV1,
    FactIdentitySourceResultV1, FactPayloadAccessV1, FactProjectionV1, FactSearchCursorV1,
    FactSearchGraphCoverageV1, FactSearchGraphDegradationV1, FactSearchHitV1, FactSearchScoresV1,
    FactStatusV1, FactStoreAddCommitV1, FactStoreAddResultV1, FactStoreContradictResultV1,
    FactStoreGetResultV1, FactStoreListResultV1, FactStoreProbeResultV1, FactStoreReasonResultV1,
    FactStoreRelatedResultV1, FactStoreRemoveResultV1, FactStoreSearchResultV1,
    FactStoreUpdateResultV1, FactTelemetryV1, FactV1, MemoryAlgebraV1, MemoryFeedbackFunnelV1,
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
    FactStoreCurate(AutomationRunResultV1),
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
    LcmExpand(Box<LcmExpandResultV1>),
    LcmExpandQuery(LcmExpandQueryResultV1),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::RetainedSurfaceResultV1;
    use super::automation::tests::{automation_request, with_request_digest};
    use crate::retained_surfaces::AutomationTaskV1;

    #[test]
    fn automation_terminal_selects_only_its_exact_result_variant() {
        let result = serde_json::from_value::<RetainedSurfaceResultV1>(with_request_digest(
            json!({
                "run_id": "run.memory.zero",
                "task": "memory_curator",
                "terminal": {
                    "status": "completed",
                    "summary": {
                        "reviewed_count": 0,
                        "accepted_count": 0,
                        "rejected_count": 0,
                        "skipped_count": 0
                    }
                },
                "committed_receipts": []
            }),
            &automation_request("run.memory.zero", AutomationTaskV1::MemoryCurator),
        ))
        .expect("canonical automation terminal");

        assert!(matches!(
            result,
            RetainedSurfaceResultV1::FactStoreCurate(_)
        ));
    }
}
