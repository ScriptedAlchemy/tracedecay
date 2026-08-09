//! Canonical memory use cases over the append-only fact authority.

use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_runtime_core::store::memory::DatabaseFactStore;
use tracedecay_store::{LegacyFactQuery, ProjectMemoryFactTargetV1};

mod anchors;
mod canonical;
mod context;
mod converge;
mod dashboard;
mod error;
mod graph;
mod legacy_fact_ids;
mod project_memory;
mod sanitize;

#[cfg(test)]
mod tests;

pub use anchors::{EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchor};
pub use context::MemoryOperationContext;
pub use error::{MemoryApplicationError, PERSISTED_FACT_ID_SOURCE_STORE, PersistedFactIdScope};
pub use legacy_fact_ids::{FactTrustHistory, MemoryStatusWithRepair, UpdateFactOutcome};
pub use project_memory::{automatic_fact_add_command, with_automation_run_id};

#[cfg(test)]
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactLineageEventV1, ProvenanceId, RetrievalAnchorRecordV2,
};
#[cfg(test)]
use tracedecay_runtime_core::memory::types::{FeedbackAction, FeedbackRequest};
#[cfg(test)]
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactStore, FactStoreError, FactWriteBatch, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptPageV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactFeedbackOutcomeV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactInspectionV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactPageV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryFeedbackRepairProgressV1, ProjectMemoryMemoryRepairCommandV1,
    ProjectMemoryMemoryRepairStatsV1, ProjectMemoryMemoryStatusV1, ProjectMemoryStoreError,
    RetrievalAnchorQuery, StoredFactV1,
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

/// Owner-bound application service. Paths, connections, and transport payloads
/// never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    persisted_fact_id_scope: PersistedFactIdScope,
    authority: A,
}

impl<A> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        Self::new_with_persisted_fact_id_scope(PersistedFactIdScope::runtime(owner)?, authority)
    }

    /// Explicit construction path for a migrated persisted source with a typed,
    /// immutable source-store identity. Callers never derive this from a path
    /// or transport field.
    pub fn new_with_persisted_fact_id_scope(
        persisted_fact_id_scope: PersistedFactIdScope,
        authority: A,
    ) -> Result<Self, MemoryApplicationError> {
        persisted_fact_id_scope.owner().validate()?;
        Ok(Self {
            owner: persisted_fact_id_scope.owner().clone(),
            persisted_fact_id_scope,
            authority,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn persisted_fact_id_scope(&self) -> &PersistedFactIdScope {
        &self.persisted_fact_id_scope
    }

    /// Decodes a shipped numeric fact identity into the canonical target used by
    /// the SQLite fact authority.
    fn persisted_fact_id_target(
        &self,
        persisted_fact_id: i64,
    ) -> Result<ProjectMemoryFactTargetV1, MemoryApplicationError> {
        LegacyFactQuery::new(
            self.owner.clone(),
            self.persisted_fact_id_scope.source_store_id().clone(),
            persisted_fact_id,
        )
        .map(ProjectMemoryFactTargetV1::Legacy)
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "persisted numeric fact target",
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
