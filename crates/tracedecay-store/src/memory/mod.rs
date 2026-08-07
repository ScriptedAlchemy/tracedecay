use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, PayloadAccessState, UtcMicros,
};

mod error;
mod project_memory;
mod queries;
mod telemetry;
mod traits;
mod write;

pub use error::{
    FactProposalStoreError, FactStoreError, FactStoreResult, ProjectMemoryResult,
    ProjectMemoryStoreError,
};
pub use project_memory::{
    FactProposalPromotionStateV1, ProjectMemoryDashboardEntityV1,
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardFactEntityLinkV1, ProjectMemoryDashboardFactSummaryV1,
    ProjectMemoryDashboardGrowthPointV1, ProjectMemoryDashboardHrrCoverageV1,
    ProjectMemoryDashboardHrrStateV1, ProjectMemoryDashboardMemoryBankV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardNamedCountV1, ProjectMemoryDashboardOplogDetailsV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactAddAliasV1, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactAvailabilityV1,
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactContradictionV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactLinkV1, ProjectMemoryFactMappingV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeEntitiesV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactNormalizeTagsV1, ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactProposalEvidenceV1, ProjectMemoryFactProposalPageV1,
    ProjectMemoryFactProposalPromotionDispositionV1,
    ProjectMemoryFactProposalPromotionResultV1, ProjectMemoryFactProposalPromotionV1,
    ProjectMemoryFactProposalRecordV1, ProjectMemoryFactProposalRevisionV1,
    ProjectMemoryFactProposalStateV1, ProjectMemoryFactRelationV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRepairVectorV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchHitV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchScoresV1,
    ProjectMemoryFactSourceV1, ProjectMemoryFactTargetV1, ProjectMemoryFactUnavailableV1,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1, ProjectMemoryLegacyEntityTargetV1,
    ProjectMemoryMemoryRepairCommandV1, ProjectMemoryRelationProvenanceV1, PromoteFactProposal,
    PromoteFactProposalOutcome,
};
pub use queries::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactContradictionStateV1,
    FactCurrentQuery, FactCurrentResponseV1, FactLineageCursor, FactLineageQuery,
    FactLineageResponseV1, FactQueryCoverageV1, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactListQueryV1, ProjectMemoryFactSearchQuery,
    RetrievalAnchorQuery,
};
pub use telemetry::{
    ProjectMemoryFactFeedbackActionV1, ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ProjectMemoryFactFeedbackHistoryEntryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactStatusV1, ProjectMemoryFactTelemetryV1, ProjectMemoryFeedbackRepairProgressV1,
    ProjectMemoryMemoryAlgebraV1, ProjectMemoryMemoryFeedbackFunnelV1,
    ProjectMemoryMemoryRepairStatsV1, ProjectMemoryMemoryStatusV1, ProjectMemoryProjectionStateV1,
};
pub use traits::{FactProposalStore, FactStore, ProjectMemoryFactStore};
pub use write::{FactCommitConflict, FactCommitOutcome, FactCommitReceipt, FactWriteBatch};

#[cfg(test)]
use project_memory::dashboard::{
    MAX_PROJECT_MEMORY_DASHBOARD_OPLOG, MAX_PROJECT_MEMORY_DASHBOARD_VECTORS,
};
#[cfg(test)]
use queries::MAX_LINEAGE_LIMIT;
#[cfg(test)]
use tracedecay_domain::{
    DomainError, FactAssertionV1, FactLineageEventKindV1, FactLineageEventV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2,
};
#[cfg(test)]
use write::{MAX_FACT_WRITE_BATCH_EVENTS, MAX_FACT_WRITE_BATCH_NEW_ANCHORS};

const MAX_PROJECT_MEMORY_SEARCH_BYTES: usize = 4 * 1024;

const MAX_PROJECT_MEMORY_REASON_BYTES: usize = 4 * 1024;

/// Deterministic current or as-of projection of one fact's lineage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredFactV1 {
    fact_id: FactId,
    owner: FactOwnerV1,
    payload: Option<FactPayloadV1>,
    payload_access: PayloadAccessState,
    trust: Confidence,
    active_assertion_id: FactAssertionId,
    last_event_id: FactEventId,
    legacy_mapping: Option<LegacyFactMappingV1>,
    projected_as_of: UtcMicros,
}

impl StoredFactV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        payload: Option<FactPayloadV1>,
        payload_access: PayloadAccessState,
        trust: Confidence,
        active_assertion_id: FactAssertionId,
        last_event_id: FactEventId,
        legacy_mapping: Option<LegacyFactMappingV1>,
        projected_as_of: UtcMicros,
    ) -> FactStoreResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        active_assertion_id.validate()?;
        last_event_id.validate()?;
        if payload.is_some() != (payload_access == PayloadAccessState::Eligible) {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        if let Some(mapping) = &legacy_mapping {
            if mapping.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if mapping.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        Ok(Self {
            fact_id,
            owner,
            payload,
            payload_access,
            trust,
            active_assertion_id,
            last_event_id,
            legacy_mapping,
            projected_as_of,
        })
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn payload(&self) -> Option<&FactPayloadV1> {
        self.payload.as_ref()
    }

    pub fn payload_access(&self) -> PayloadAccessState {
        self.payload_access
    }

    pub fn trust(&self) -> Confidence {
        self.trust
    }

    pub fn active_assertion_id(&self) -> &FactAssertionId {
        &self.active_assertion_id
    }

    pub fn last_event_id(&self) -> &FactEventId {
        &self.last_event_id
    }

    pub fn legacy_mapping(&self) -> Option<&LegacyFactMappingV1> {
        self.legacy_mapping.as_ref()
    }

    pub fn projected_as_of(&self) -> UtcMicros {
        self.projected_as_of
    }
}

fn validate_owned_fact_id(fact_id: &FactId, owner: &FactOwnerV1) -> FactStoreResult<()> {
    fact_id
        .validate_owner(owner)
        .map_err(|_| FactStoreError::OwnerMismatch)
}

#[cfg(test)]
mod tests;
