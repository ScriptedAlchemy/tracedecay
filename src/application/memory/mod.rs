//! Canonical memory use cases over the append-only fact authority.

use tracedecay_domain::{FactId, FactLineageEventV1, FactOwnerV1, RetrievalAnchorRecordV2};
use tracedecay_store::{
    CompatibilityFactTargetV1, CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome,
    FactCurrentQuery, FactLineageQuery, FactProposalStore, FactStore, FactWriteBatch,
    LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery,
    StoredFactV1,
};

use compatibility::validate_lineage;

mod anchors;
mod compatibility;
mod context;
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
use crate::memory::types::{FeedbackAction, FeedbackRequest};
#[cfg(test)]
use tracedecay_domain::{ActorId, DomainError, ProvenanceId};
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
    CompatibilityLegacyMemoryCutoverCommandV1, CompatibilityLegacyMemoryCutoverProgressV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, FactCompatibilityStore, FactCompatibilityStoreError,
    FactProposalStoreError, FactStoreError,
};

/// Owner-bound application service. Paths, connections, legacy integer IDs,
/// and transport payloads never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    compatibility_scope: MemoryCompatibilityScope,
    authority: A,
}

impl<A: FactStore> MemoryApplication<A> {
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

    pub async fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> Result<FactCommitOutcome, MemoryApplicationError> {
        self.ensure_owner(batch.owner())?;
        let expected_fact_id = batch.fact_id().clone();
        let outcome = self.authority.commit_fact(batch).await?;
        validate_commit_outcome(&self.owner, &expected_fact_id, &outcome)?;
        Ok(outcome)
    }

    pub async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> Result<Vec<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let facts = self.authority.query_current_facts(query).await?;
        validate_current_facts(&self.owner, after_fact_id.as_ref(), limit, &facts)?;
        Ok(facts)
    }

    pub async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let as_of = query.as_of();
        let fact = self.authority.query_fact_as_of(query).await?;
        if let Some(fact) = &fact
            && (fact.owner() != &self.owner
                || fact.fact_id() != &fact_id
                || fact.projected_as_of() > as_of)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "as-of fact identity and timestamp",
            });
        }
        Ok(fact)
    }

    pub async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let fact = self.authority.query_fact_current(query).await?;
        if let Some(fact) = &fact
            && (fact.owner() != &self.owner || fact.fact_id() != &fact_id)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "current fact identity",
            });
        }
        Ok(fact)
    }

    pub async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> Result<Vec<FactLineageEventV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let events = self.authority.query_fact_lineage(query).await?;
        validate_lineage(&self.owner, &fact_id, after.as_ref(), limit, &events)?;
        Ok(events)
    }

    pub async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> Result<Option<FactId>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = self.authority.resolve_legacy_fact(query).await?;
        if fact_id
            .as_ref()
            .is_some_and(|fact_id| fact_id.validate_owner(&self.owner).is_err())
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "legacy fact owner",
            });
        }
        Ok(fact_id)
    }

    pub async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let anchor_id = query.anchor_id().clone();
        let anchor = self.authority.get_retrieval_anchor(query).await?;
        if let Some(anchor) = &anchor
            && (anchor.anchor_id() != &anchor_id
                || FactOwnerV1::from(anchor.owner().clone()) != self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "retrieval anchor identity",
            });
        }
        Ok(anchor)
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

impl<A: FactProposalStore> MemoryApplication<A> {
    pub async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, MemoryApplicationError> {
        self.ensure_owner(promotion.owner())?;
        let proposal_id = promotion.proposal_id().clone();
        let previous_state = promotion.expected_state();
        let fact_id = promotion.batch().fact_id().clone();
        let outcome = self.authority.promote_fact_proposal(promotion).await?;
        if outcome.proposal_id() != &proposal_id || outcome.previous_state() != previous_state {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "proposal CAS identity",
            });
        }
        validate_commit_outcome(&self.owner, &fact_id, outcome.commit())?;
        Ok(outcome)
    }
}

fn validate_commit_outcome(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    outcome: &FactCommitOutcome,
) -> Result<(), MemoryApplicationError> {
    let receipt = match outcome {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            Some(receipt)
        }
        FactCommitOutcome::Conflict(_) => None,
        _ => {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "recognized fact commit outcome",
            });
        }
    };
    if receipt.is_some_and(|receipt| receipt.owner() != owner || receipt.fact_id() != fact_id) {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "fact commit identity",
        });
    }
    Ok(())
}

fn validate_current_facts(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    facts: &[StoredFactV1],
) -> Result<(), MemoryApplicationError> {
    if facts.len() > limit
        || facts.iter().any(|fact| fact.owner() != owner)
        || after_fact_id
            .is_some_and(|after_fact_id| facts.iter().any(|fact| fact.fact_id() <= after_fact_id))
        || facts
            .windows(2)
            .any(|pair| pair[0].fact_id() >= pair[1].fact_id())
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "current fact bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}
