//! Canonical memory use cases over the append-only fact authority.

use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_runtime_core::store::memory::DatabaseFactStore;

mod anchors;
mod canonical;
mod compatibility;
mod context;
mod dashboard;
mod error;
mod sanitize;

#[cfg(test)]
mod tests;

pub use anchors::{
    EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchorV1,
};
pub use compatibility::{automation_fact_proposal_add_command, with_automation_run_id};
pub use context::MemoryOperationContext;
pub use error::{
    MemoryApplicationError, MemoryCompatibilityScope, RUNTIME_MEMORY_COMPATIBILITY_SOURCE_STORE,
};

#[cfg(test)]
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactLineageEventV1, ProvenanceId, RetrievalAnchorRecordV2,
};
#[cfg(test)]
use tracedecay_runtime_core::memory::types::{FeedbackAction, FeedbackRequest};
#[cfg(test)]
use tracedecay_store::{
    CurrentFactsQuery, DashboardFactDetail, DashboardFactDetailQuery, DashboardMemoryOverview,
    DashboardMemoryOverviewQuery, DashboardOplogEntry, DashboardOplogQuery, DashboardVectorPoint,
    DashboardVectorPointsQuery, FactAddCommand, FactAddOutcome, FactAsOfQuery, FactCommitOutcome,
    FactContentDigestQuery, FactContradictionPage, FactContradictionQuery, FactCurationBatch,
    FactCurationReceipt, FactCurrentQuery, FactFeedbackCommand, FactFeedbackHistory,
    FactFeedbackHistoryQuery, FactFeedbackOutcome, FactHistory, FactHistoryQuery, FactInspection,
    FactLineageError, FactLineageQuery, FactLineageStore, FactListQuery, FactMergeCommand,
    FactMergeOutcome, FactPage, FactProjection, FactProposalPage, FactProposalPromotion,
    FactProposalPromotionResult, FactProposalRecord, FactProposalRevision, FactProposalState,
    FactProposalStore, FactProposalStoreError, FactRemoveCommand, FactRemoveOutcome,
    FactRetrievalCommand, FactSearchPage, FactSearchQuery, FactStore, FactStoreError,
    FactUpdateCommand, FactUpdateOutcome, FactWriteBatch, FeedbackRepairProgress,
    MemoryRepairCommand, MemoryRepairStats, MemoryStatus, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
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
    scope: MemoryCompatibilityScope,
    authority: A,
}

impl<A> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        Self::new_with_scope(MemoryCompatibilityScope::runtime(owner)?, authority)
    }

    /// Explicit construction path for a migrated V1 source with a typed,
    /// immutable source-store identity. Callers never derive this from a path
    /// or transport field.
    pub fn new_with_scope(
        scope: MemoryCompatibilityScope,
        authority: A,
    ) -> Result<Self, MemoryApplicationError> {
        scope.owner().validate()?;
        Ok(Self {
            owner: scope.owner().clone(),
            scope,
            authority,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn scope(&self) -> &MemoryCompatibilityScope {
        &self.scope
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
