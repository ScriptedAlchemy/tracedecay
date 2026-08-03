//! Canonical memory use cases over the append-only fact authority.

use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_runtime_core::store::memory::DatabaseFactStore;
use tracedecay_store::{CompatibilityFactTargetV1, LegacyFactQuery};

mod anchors;
mod canonical;
mod compatibility;
mod context;
mod converge;
mod dashboard;
mod error;
mod sanitize;
mod v1;

#[cfg(test)]
mod tests;

pub use anchors::{
    EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchorV1,
};
pub use compatibility::{
    automation_fact_proposal_add_command, legacy_proposal_add_command, with_automation_run_id,
};
pub use context::MemoryOperationContext;
pub use error::{
    MemoryApplicationError, MemoryCompatibilityScope, RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE,
};
pub use v1::{V1FactTrustHistoryV1, V1MemoryStatusWithRepairV1, V1UpdateFactOutcome};

#[cfg(test)]
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactLineageEventV1, ProvenanceId, RetrievalAnchorRecordV2,
};
#[cfg(test)]
use tracedecay_runtime_core::memory::types::{FeedbackAction, FeedbackRequest};
#[cfg(test)]
use tracedecay_store::{
    CompatibilityDashboardFactDetailQueryV1, CompatibilityDashboardFactDetailV1,
    CompatibilityDashboardMemoryOverviewQueryV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityDashboardOplogEntryV1, CompatibilityDashboardOplogQueryV1,
    CompatibilityDashboardVectorPointV1, CompatibilityDashboardVectorPointsQueryV1,
    CompatibilityFactAddCommandV1, CompatibilityFactAddOutcomeV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationReceiptV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackHistoryQueryV1, CompatibilityFactFeedbackHistoryV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryQueryV1,
    CompatibilityFactHistoryV1, CompatibilityFactInspectionV1, CompatibilityFactListQueryV1,
    CompatibilityFactMergeCommandV1, CompatibilityFactMergeOutcomeV1, CompatibilityFactPageV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalImportReceiptV1,
    CompatibilityFactProposalImportV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactRetrievalCommandV1,
    CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome,
    FactCompatibilityStore, FactCompatibilityStoreError, FactCurrentQuery, FactLineageQuery,
    FactProposalStore, FactProposalStoreError, FactStore, FactStoreError, FactWriteBatch,
    PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

/// Maps a [`MemoryApplicationError`] onto the root/dashboard-facing
/// [`TraceDecayError`]. The single conversion site for every project-memory
/// route across the root crate and the dashboard API, so both stay in sync
/// instead of maintaining independent copies.
pub fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    TraceDecayError::database_operation("memory application", error)
}

/// Builds a [`MemoryApplication`] directly over a database handle's
/// [`DatabaseFactStore`]. The shared resolver for every route that already
/// holds an open [`Database`] rather than a higher-level fact-store handle —
/// used by the root crate's daemon scheduler and MCP lifecycle paths as well
/// as the dashboard API.
pub fn memory_application_for_db(
    owner: FactOwnerV1,
    db: &Database,
) -> TraceDecayResult<MemoryApplication<DatabaseFactStore<'_>>> {
    MemoryApplication::new(owner, DatabaseFactStore::new(db)).map_err(memory_application_error)
}

/// Owner-bound application service. Paths, connections, legacy integer IDs,
/// and transport payloads never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    compatibility_scope: MemoryCompatibilityScope,
    authority: A,
}

impl<A> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        Self::new_with_compatibility_scope(MemoryCompatibilityScope::runtime(owner)?, authority)
    }

    /// Explicit construction path for a migrated V1 source with a typed,
    /// immutable source-store identity. Callers never derive this from a path
    /// or transport field.
    pub fn new_with_compatibility_scope(
        compatibility_scope: MemoryCompatibilityScope,
        authority: A,
    ) -> Result<Self, MemoryApplicationError> {
        compatibility_scope.owner().validate()?;
        Ok(Self {
            owner: compatibility_scope.owner().clone(),
            compatibility_scope,
            authority,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn compatibility_scope(&self) -> &MemoryCompatibilityScope {
        &self.compatibility_scope
    }

    fn legacy_compatibility_target(
        &self,
        legacy_fact_id: i64,
    ) -> Result<CompatibilityFactTargetV1, MemoryApplicationError> {
        LegacyFactQuery::new(
            self.owner.clone(),
            self.compatibility_scope.source_store_id().clone(),
            legacy_fact_id,
        )
        .map(CompatibilityFactTargetV1::Legacy)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy numeric fact target",
        })
    }

    fn ensure_owner(&self, request_owner: &FactOwnerV1) -> Result<(), MemoryApplicationError> {
        request_owner.validate()?;
        if request_owner != &self.owner {
            return Err(MemoryApplicationError::OwnerMismatch {
                scope: self.owner.clone(),
                request_owner: request_owner.clone(),
            });
        }
        Ok(())
    }
}
