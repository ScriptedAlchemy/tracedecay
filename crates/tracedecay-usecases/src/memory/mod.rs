//! Canonical memory use cases over the append-only fact authority.

use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::{Result as TraceDecayResult, TraceDecayError};
use tracedecay_runtime_core::store::memory::DatabaseFactStore;

mod anchors;
mod canonical;
mod context;
mod curation;
mod dashboard;
mod error;
mod graph;
mod privacy_remediation;
mod project_memory;
mod sanitize;

#[cfg(test)]
mod tests;

pub use anchors::{EvidenceAnchorResolutionError, EvidenceAnchorResolver, ResolvedEvidenceAnchor};
pub use context::MemoryOperationContext;
pub use curation::{
    ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation,
    ProjectMemoryFactMutationTarget,
};
pub use error::{MemoryApplicationError, MemoryMutationError};
pub use privacy_remediation::{
    PrivacyRemediationTriggerV1, ProjectMemoryPrivacyRemediationReceiptV1,
};
pub use project_memory::{
    ProjectMemoryFactAddEffectMaterialV1, ProjectMemoryFactAddPreflight,
    ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome, automatic_fact_add_command,
};

#[cfg(test)]
use tracedecay_domain::{
    DomainError, FactId, FactLineageEventV1, ProvenanceId, RetrievalAnchorRecordV2,
};
#[cfg(test)]
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactReadControl, FactStore, FactStoreError, FactWriteBatch, FactWriteControl,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptPageV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryAutomaticFactStateV1, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1, ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactContentDigestQueryV1,
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactHistoryV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactRetrievalOutcomeV1, ProjectMemoryFactSearchGraphCoverageV1,
    ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryMemoryStatusV1, RetrievalAnchorQuery, StoredFactV1,
};

/// Maps a [`MemoryApplicationError`] onto the root/dashboard-facing
/// [`TraceDecayError`]. The single conversion site for every project-memory
/// route across the root crate and the dashboard API, so both stay in sync
/// instead of maintaining independent copies.
pub fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    match error {
        MemoryApplicationError::Store(tracedecay_store::FactStoreError::GraphResetRequired {
            owner,
            reason,
        }) => {
            let authority = match owner {
                FactOwnerV1::Profile => "profile memory graph",
                FactOwnerV1::Project { .. } => "project memory graph",
            };
            TraceDecayError::reset_required(authority, reason)
        }
        error => TraceDecayError::database_operation("memory application", error),
    }
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
    authority: A,
}

impl<A> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        Ok(Self { owner, authority })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
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
