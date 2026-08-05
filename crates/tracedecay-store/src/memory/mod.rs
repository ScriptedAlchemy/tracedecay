use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, PayloadAccessState, UtcMicros,
};

mod compatibility;
mod error;
mod queries;
mod telemetry;
mod traits;
mod write;

pub use compatibility::{
    DashboardEntity, DashboardFactDetail, DashboardFactDetailQuery, DashboardFactEntityLink,
    DashboardFactSummary, DashboardGrowthPoint, DashboardHrrCoverage, DashboardHrrState,
    DashboardMemoryBank, DashboardMemoryOverview, DashboardMemoryOverviewQuery,
    DashboardNamedCount, DashboardOplogDetails, DashboardOplogEntry, DashboardOplogQuery,
    DashboardVectorPoint, DashboardVectorPointsQuery, Fact, FactAddAlias, FactAddCommand,
    FactAddDisposition, FactAddOutcome, FactAvailability, FactContradiction, FactContradictionPage,
    FactContradictionQuery, FactCurationBatch, FactCurationOperation, FactCurationReceipt,
    FactEntityTarget, FactFeedbackCommand, FactFeedbackOutcome, FactHistory, FactInspection,
    FactLink, FactMapping, FactMergeCommand, FactMergeEntities, FactMergeOutcome,
    FactNormalizeTags, FactPage, FactProjection, FactProposalEvidence, FactProposalPage,
    FactProposalPromotion, FactProposalPromotionDisposition, FactProposalPromotionResult,
    FactProposalPromotionStateV1, FactProposalRecord, FactProposalRevision, FactProposalState,
    FactRelation, FactRemoveCommand, FactRemoveOutcome, FactRepairVector, FactRetrievalCommand,
    FactSearchCursor, FactSearchFilter, FactSearchHit, FactSearchKind, FactSearchPage,
    FactSearchScores, FactSource, FactTarget, FactUnavailable, FactUpdateCommand,
    FactUpdateOutcome, FactUpdatePatch, MemoryRepairCommand, OwnedFactId, PromoteFactProposal,
    PromoteFactProposalOutcome,
};
pub use error::{
    FactLineageError, FactLineageResult, FactProposalStoreError, FactStoreError, FactStoreResult,
};
pub use queries::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactContentDigestQuery,
    FactContradictionStateV1, FactCurrentQuery, FactCurrentResponseV1, FactFeedbackHistoryQuery,
    FactHistoryQuery, FactLineageCursor, FactLineageQuery, FactLineageResponseV1, FactListQuery,
    FactQueryCoverageV1, FactSearchQuery, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS,
    RetrievalAnchorQuery,
};
pub use telemetry::{
    FactFeedbackAction, FactFeedbackDetailsAvailability, FactFeedbackHistory,
    FactFeedbackHistoryEntry, FactStatus, FactTelemetry, FeedbackRepairProgress, MemoryAlgebra,
    MemoryFeedbackFunnel, MemoryRepairStats, MemoryStatus, ProjectionState,
};
pub use traits::{FactLineageStore, FactProposalStore, FactStore};
pub use write::{FactCommitConflict, FactCommitOutcome, FactCommitReceipt, FactWriteBatch};

#[cfg(test)]
use compatibility::dashboard::{MAX_FACT_DASHBOARD_OPLOG, MAX_FACT_DASHBOARD_VECTORS};
#[cfg(test)]
use queries::MAX_LINEAGE_LIMIT;
#[cfg(test)]
use tracedecay_domain::{
    DomainError, FactAssertionV1, FactLineageEventKindV1, FactLineageEventV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2,
};
#[cfg(test)]
use write::{MAX_FACT_WRITE_BATCH_EVENTS, MAX_FACT_WRITE_BATCH_NEW_ANCHORS};

const MAX_FACT_SEARCH_BYTES: usize = 4 * 1024;

const MAX_FACT_REASON_BYTES: usize = 4 * 1024;

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
    ) -> FactLineageResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        active_assertion_id.validate()?;
        last_event_id.validate()?;
        if payload.is_some() != (payload_access == PayloadAccessState::Eligible) {
            return Err(FactLineageError::PayloadAccessMismatch);
        }
        if let Some(mapping) = &legacy_mapping {
            if mapping.fact_id() != &fact_id {
                return Err(FactLineageError::FactMismatch);
            }
            if mapping.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
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

fn validate_owned_fact_id(fact_id: &FactId, owner: &FactOwnerV1) -> FactLineageResult<()> {
    fact_id
        .validate_owner(owner)
        .map_err(|_| FactLineageError::OwnerMismatch)
}

#[cfg(test)]
mod tests;
