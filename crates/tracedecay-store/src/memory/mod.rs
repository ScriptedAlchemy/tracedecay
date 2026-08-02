use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, PayloadAccessState, UtcMicros,
};

mod archive;
mod compatibility;
mod error;
mod queries;
mod telemetry;
mod traits;
mod write;

pub use archive::{
    MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1, MemoryV2ArchiveConflictV1, MemoryV2ArchiveError,
    MemoryV2ArchiveFamilyV1, MemoryV2ArchiveRecordV1, MemoryV2ArchiveReferenceV1,
    MemoryV2ArchiveScalarV1, MemoryV2OwnerArchiveV1, MemoryV2OwnerMergePlanV1,
    authoritative_memory_v2_archive_families, plan_memory_v2_owner_merge,
};
pub use compatibility::{
    CompatibilityDashboardEntityV1, CompatibilityDashboardFactDetailQueryV1,
    CompatibilityDashboardFactDetailV1, CompatibilityDashboardFactEntityLinkV1,
    CompatibilityDashboardFactSummaryV1, CompatibilityDashboardGrowthPointV1,
    CompatibilityDashboardHrrCoverageV1, CompatibilityDashboardHrrStateV1,
    CompatibilityDashboardMemoryBankV1, CompatibilityDashboardMemoryOverviewQueryV1,
    CompatibilityDashboardMemoryOverviewV1, CompatibilityDashboardNamedCountV1,
    CompatibilityDashboardOplogDetailsV1, CompatibilityDashboardOplogEntryV1,
    CompatibilityDashboardOplogQueryV1, CompatibilityDashboardVectorPointV1,
    CompatibilityDashboardVectorPointsQueryV1, CompatibilityFactAddAliasV1,
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactAvailabilityV1,
    CompatibilityFactContradictionPageV1, CompatibilityFactContradictionQueryV1,
    CompatibilityFactContradictionV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationOperationV1, CompatibilityFactCurationReceiptV1,
    CompatibilityFactFeedbackCommandV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryV1, CompatibilityFactIdV1, CompatibilityFactInspectionV1,
    CompatibilityFactLinkV1, CompatibilityFactMappingV1, CompatibilityFactMergeCommandV1,
    CompatibilityFactMergeEntitiesV1, CompatibilityFactMergeOutcomeV1,
    CompatibilityFactNormalizeTagsV1, CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalLegacyRecordV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionDispositionV1, CompatibilityFactProposalPromotionResultV1,
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1,
    CompatibilityFactRelationV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactRepairVectorV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchFilterV1, CompatibilityFactSearchHitV1, CompatibilityFactSearchKindV1,
    CompatibilityFactSearchPageV1, CompatibilityFactSearchScoresV1, CompatibilityFactSourceV1,
    CompatibilityFactTargetV1, CompatibilityFactUnavailableV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFactUpdatePatchV1, CompatibilityFactV1,
    CompatibilityLegacyEntityTargetV1, CompatibilityMemoryRepairCommandV1,
    FactProposalPromotionStateV1, PromoteFactProposal, PromoteFactProposalOutcome,
};
pub use error::{
    FactCompatibilityResult, FactCompatibilityStoreError, FactProposalStoreError, FactStoreError,
    FactStoreResult,
};
pub use queries::{
    CompatibilityFactContentDigestQueryV1, CompatibilityFactFeedbackHistoryQueryV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactListQueryV1, CompatibilityFactSearchQuery,
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactContradictionStateV1,
    FactCurrentQuery, FactCurrentResponseV1, FactLineageCursor, FactLineageQuery,
    FactLineageResponseV1, FactQueryCoverageV1, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS,
    RetrievalAnchorQuery,
};
pub use telemetry::{
    CompatibilityFactFeedbackActionV1, CompatibilityFactFeedbackDetailsAvailabilityV1,
    CompatibilityFactFeedbackHistoryEntryV1, CompatibilityFactFeedbackHistoryV1,
    CompatibilityFactStatusV1, CompatibilityFactTelemetryV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityMemoryAlgebraV1, CompatibilityMemoryFeedbackFunnelV1,
    CompatibilityMemoryRepairStatsV1, CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1,
};
pub use traits::{FactCompatibilityStore, FactProposalStore, FactStore};
pub use write::{FactCommitConflict, FactCommitOutcome, FactCommitReceipt, FactWriteBatch};

#[cfg(test)]
use compatibility::dashboard::{
    MAX_COMPATIBILITY_DASHBOARD_OPLOG, MAX_COMPATIBILITY_DASHBOARD_VECTORS,
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

const MAX_COMPATIBILITY_SEARCH_BYTES: usize = 4 * 1024;

const MAX_COMPATIBILITY_REASON_BYTES: usize = 4 * 1024;

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
