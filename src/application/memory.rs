//! Canonical memory use cases over the append-only fact authority.

use std::error::Error as StdError;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorId,
    RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactAddOutcomeV1,
    CompatibilityFactContradictionPageV1, CompatibilityFactContradictionQueryV1,
    CompatibilityFactFeedbackCommandV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactHistoryV1, CompatibilityFactInspectionV1,
    CompatibilityFactListQueryV1, CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactRetrievalCommandV1,
    CompatibilityFactSearchCursorV1, CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery,
    CompatibilityFactTargetV1, CompatibilityFactUpdateCommandV1, CompatibilityFactUpdateOutcomeV1,
    CompatibilityMemoryStatusV1, CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome,
    FactCompatibilityStore, FactCompatibilityStoreError, FactCurrentQuery, FactLineageQuery,
    FactProposalStore, FactProposalStoreError, FactStore, FactStoreError, FactWriteBatch,
    LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery,
    StoredFactV1,
};

#[derive(Debug, Error)]
pub enum MemoryApplicationError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("evidence anchor is invalid")]
    InvalidEvidenceAnchor(#[source] DomainError),
    #[error("memory request owner does not match the application scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        request_owner: FactOwnerV1,
    },
    #[error("fact store operation failed")]
    Store(#[from] FactStoreError),
    #[error("memory authority operation failed")]
    Authority(#[from] FactProposalStoreError),
    #[error("memory compatibility authority operation failed")]
    Compatibility(#[from] FactCompatibilityStoreError),
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
    #[error("evidence anchor resolution failed")]
    EvidenceAnchor(#[from] EvidenceAnchorResolutionError),
}

/// Immutable daemon-authorized evidence record suitable for materialization in
/// a fact shard. It deliberately reuses the canonical retrieval-anchor model.
#[derive(Clone, Debug)]
pub struct ResolvedEvidenceAnchorV1 {
    record: RetrievalAnchorRecordV2,
}

impl ResolvedEvidenceAnchorV1 {
    pub fn new(record: RetrievalAnchorRecordV2) -> Result<Self, DomainError> {
        record.validate()?;
        Ok(Self { record })
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        self.record.anchor_id()
    }

    pub fn record(&self) -> &RetrievalAnchorRecordV2 {
        &self.record
    }

    pub fn into_record(self) -> RetrievalAnchorRecordV2 {
        self.record
    }
}

#[derive(Debug, Error)]
pub enum EvidenceAnchorResolutionError {
    #[error("evidence anchor {anchor_id} is unavailable from the daemon authority")]
    Unavailable { anchor_id: RetrievalAnchorId },
    #[error("evidence anchor resolver operation {operation} failed")]
    Authority {
        operation: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
}

/// Daemon/ingress-only boundary for resolving observation evidence that lives
/// outside the fact shard. Implementations must not expose a database handle.
pub trait EvidenceAnchorResolver: Send + Sync {
    fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> impl Future<Output = Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError>> + Send;
}

/// Owner-bound application service. Paths, connections, legacy integer IDs,
/// and transport payloads never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    authority: A,
}

impl<A: FactStore> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        Ok(Self { owner, authority })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    /// Resolves a daemon-authorized observation anchor before the caller
    /// materializes the returned record in `FactWriteBatch::new_anchors`.
    /// The fact shard never performs a cross-database anchor lookup itself.
    pub async fn resolve_evidence_anchor<R: EvidenceAnchorResolver>(
        &self,
        resolver: &R,
        anchor_id: RetrievalAnchorId,
    ) -> Result<RetrievalAnchorRecordV2, MemoryApplicationError> {
        anchor_id
            .validate()
            .map_err(MemoryApplicationError::InvalidEvidenceAnchor)?;
        let resolved = resolver
            .resolve_evidence_anchor(self.owner.clone(), anchor_id.clone())
            .await?;
        let record = resolved.into_record();
        if record.anchor_id() != &anchor_id
            || FactOwnerV1::from(record.owner().clone()) != self.owner
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner",
            });
        }
        Ok(record)
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

/// Typed compatibility use cases. Transport adapters translate legacy inputs
/// before this boundary; only the authority owns the corresponding mutation
/// transaction and compatibility projection.
impl<A: FactCompatibilityStore> MemoryApplication<A> {
    pub async fn list_compatibility_facts(
        &self,
        query: CompatibilityFactListQueryV1,
    ) -> Result<CompatibilityFactPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        let page = self.authority.list_compatibility_facts(query).await?;
        validate_compatibility_page(&self.owner, after_fact_id.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.search_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.probe_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.related_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let after = query.after().cloned();
        let limit = query.limit();
        let page = self.authority.reason_compatibility_facts(query).await?;
        validate_compatibility_search_page(&self.owner, after.as_ref(), limit, &page)?;
        Ok(page)
    }

    pub async fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> Result<CompatibilityFactContradictionPageV1, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let limit = query.limit();
        let page = self
            .authority
            .find_compatibility_contradictions(query)
            .await?;
        if page.owner() != &self.owner
            || page.contradictions().len() > limit
            || page
                .contradictions()
                .iter()
                .any(|contradiction| contradiction.existing().owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility contradiction bounds and owner",
            });
        }
        Ok(page)
    }

    pub async fn get_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let result = self
            .authority
            .get_compatibility_fact(target.clone())
            .await?;
        if let Some(projection) = &result {
            validate_compatibility_projection(&self.owner, &target, projection)?;
        }
        Ok(result)
    }

    pub async fn get_compatibility_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> Result<CompatibilityFactHistoryV1, MemoryApplicationError> {
        self.ensure_owner(query.target().owner())?;
        let target = query.target().clone();
        let after = query.after().cloned();
        let limit = query.limit();
        let history = self.authority.compatibility_fact_history(query).await?;
        if history.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility history owner",
            });
        }
        if let Some(fact_id) = target.canonical_fact_id()
            && history.fact_id() != fact_id
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility history canonical identity",
            });
        }
        validate_lineage(
            &self.owner,
            history.fact_id(),
            after.as_ref(),
            limit,
            history.events(),
        )?;
        Ok(history)
    }

    pub async fn compatibility_memory_status(
        &self,
    ) -> Result<CompatibilityMemoryStatusV1, MemoryApplicationError> {
        let status = self
            .authority
            .compatibility_memory_status(self.owner.clone())
            .await?;
        if status.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility memory status owner",
            });
        }
        Ok(status)
    }

    pub async fn inspect_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactInspectionV1>, MemoryApplicationError> {
        self.ensure_owner(target.owner())?;
        let inspection = self
            .authority
            .inspect_compatibility_fact(target.clone())
            .await?;
        if let Some(inspection) = &inspection {
            validate_compatibility_inspection(&self.owner, &target, inspection)?;
        }
        Ok(inspection)
    }

    pub async fn add_compatibility_fact(
        &self,
        request: CompatibilityFactAddCommandV1,
    ) -> Result<CompatibilityFactAddOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.add_compatibility_fact(request).await?;
        validate_compatibility_add_outcome(&self.owner, &outcome)?;
        Ok(outcome)
    }

    pub async fn update_compatibility_fact(
        &self,
        request: CompatibilityFactUpdateCommandV1,
    ) -> Result<CompatibilityFactUpdateOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.update_compatibility_fact(request).await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn remove_compatibility_fact(
        &self,
        request: CompatibilityFactRemoveCommandV1,
    ) -> Result<CompatibilityFactRemoveOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self.authority.remove_compatibility_fact(request).await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> Result<CompatibilityFactFeedbackOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.target().owner())?;
        let target = request.target().clone();
        let outcome = self
            .authority
            .record_compatibility_fact_feedback(request)
            .await?;
        validate_compatibility_projection(&self.owner, &target, outcome.fact())?;
        Ok(outcome)
    }

    pub async fn record_compatibility_fact_retrieval(
        &self,
        request: CompatibilityFactRetrievalCommandV1,
    ) -> Result<Vec<CompatibilityFactProjectionV1>, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let targets = request.targets().to_vec();
        let projections = self
            .authority
            .record_compatibility_fact_retrieval(request)
            .await?;
        if projections
            .iter()
            .any(|projection| projection.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility retrieval projection owner",
            });
        }
        for target in &targets {
            if let Some(fact_id) = target.canonical_fact_id()
                && projections
                    .iter()
                    .any(|projection| projection.fact_id() == fact_id)
            {
                continue;
            }
            if target.canonical_fact_id().is_some() {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "compatibility retrieval canonical target",
                });
            }
        }
        Ok(projections)
    }

    pub async fn submit_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: CompatibilityFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let proposal = self
            .authority
            .submit_compatibility_fact_proposal(proposal_id.clone(), request, submitter)
            .await?;
        validate_compatibility_proposal(&self.owner, &proposal_id, &proposal)?;
        Ok(proposal)
    }

    pub async fn get_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
    ) -> Result<Option<CompatibilityFactProposalRecordV1>, MemoryApplicationError> {
        let proposal = self
            .authority
            .get_compatibility_fact_proposal(self.owner.clone(), proposal_id.clone())
            .await?;
        if let Some(proposal) = &proposal {
            validate_compatibility_proposal(&self.owner, &proposal_id, proposal)?;
        }
        Ok(proposal)
    }

    pub async fn list_compatibility_fact_proposals(
        &self,
        state: Option<CompatibilityFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> Result<CompatibilityFactProposalPageV1, MemoryApplicationError> {
        let page = self
            .authority
            .list_compatibility_fact_proposals(
                self.owner.clone(),
                state,
                after_proposal_id.clone(),
                limit,
            )
            .await?;
        validate_compatibility_proposal_page(
            &self.owner,
            after_proposal_id.as_ref(),
            limit,
            &page,
        )?;
        Ok(page)
    }

    pub async fn count_pending_compatibility_fact_proposals(
        &self,
    ) -> Result<u64, MemoryApplicationError> {
        Ok(self
            .authority
            .count_pending_compatibility_fact_proposals(self.owner.clone())
            .await?)
    }

    pub async fn reject_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        expected_revision: CompatibilityFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        let proposal = self
            .authority
            .reject_compatibility_fact_proposal(
                self.owner.clone(),
                proposal_id.clone(),
                expected_revision,
                reviewer,
                reason,
            )
            .await?;
        validate_compatibility_proposal(&self.owner, &proposal_id, &proposal)?;
        if proposal.revision() <= expected_revision {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal rejection revision",
            });
        }
        Ok(proposal)
    }

    pub async fn import_legacy_compatibility_fact_proposals(
        &self,
        request: CompatibilityFactProposalImportV1,
    ) -> Result<CompatibilityFactProposalImportReceiptV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let source_store_id = request.source_store_id().clone();
        let sidecar_digest = request.sidecar_digest().clone();
        let receipt = self
            .authority
            .import_legacy_compatibility_fact_proposals(request)
            .await?;
        if receipt.owner() != &self.owner
            || receipt.source_store_id() != &source_store_id
            || receipt.sidecar_digest() != &sidecar_digest
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal import identity",
            });
        }
        Ok(receipt)
    }

    pub async fn promote_compatibility_fact_proposal(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> Result<CompatibilityFactProposalRecordV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let proposal_id = request.proposal_id().clone();
        let expected_revision = request.expected_revision();
        let proposal = self
            .authority
            .promote_compatibility_fact_proposal(request)
            .await?;
        validate_compatibility_proposal(&self.owner, &proposal_id, &proposal)?;
        if proposal.revision() <= expected_revision {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility proposal promotion revision",
            });
        }
        Ok(proposal)
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

fn validate_compatibility_page(
    owner: &FactOwnerV1,
    after_fact_id: Option<&FactId>,
    limit: usize,
    page: &CompatibilityFactPageV1,
) -> Result<(), MemoryApplicationError> {
    let facts = page.facts();
    let cursor_is_invalid = page.next_after_fact_id().is_some_and(|cursor| {
        cursor.validate_owner(owner).is_err()
            || after_fact_id.is_some_and(|after| cursor <= after)
            || facts.last().is_none_or(|last| cursor <= last.fact_id())
    });
    if page.owner() != owner
        || facts.len() > limit
        || facts.iter().any(|fact| fact.owner() != owner)
        || after_fact_id.is_some_and(|after| facts.iter().any(|fact| fact.fact_id() <= after))
        || facts
            .windows(2)
            .any(|pair| pair[0].fact_id() >= pair[1].fact_id())
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility list bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_compatibility_search_page(
    owner: &FactOwnerV1,
    after: Option<&CompatibilityFactSearchCursorV1>,
    limit: usize,
    page: &CompatibilityFactSearchPageV1,
) -> Result<(), MemoryApplicationError> {
    let hits = page.hits();
    let cursor_is_invalid = page.next_after().is_some_and(|cursor| {
        cursor.fact_id().validate_owner(owner).is_err()
            || hits.last().is_none_or(|last| {
                cursor.score_millionths() != last.score_millionths()
                    || cursor.updated_at() != last.fact().telemetry().updated_at()
                    || cursor.fact_id() != last.fact().fact_id()
            })
    });
    if page.owner() != owner
        || hits.len() > limit
        || hits.iter().any(|hit| hit.fact().owner() != owner)
        || after.is_some_and(|after| {
            hits.iter()
                .any(|hit| !search_hit_follows_cursor(hit, after))
        })
        || hits.windows(2).any(|pair| {
            pair[0].score_millionths() < pair[1].score_millionths()
                || (pair[0].score_millionths() == pair[1].score_millionths()
                    && (pair[0].fact().telemetry().updated_at()
                        < pair[1].fact().telemetry().updated_at()
                        || (pair[0].fact().telemetry().updated_at()
                            == pair[1].fact().telemetry().updated_at()
                            && pair[0].fact().fact_id() >= pair[1].fact().fact_id())))
        })
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility search bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn search_hit_follows_cursor(
    hit: &tracedecay_store::CompatibilityFactSearchHitV1,
    after: &CompatibilityFactSearchCursorV1,
) -> bool {
    hit.score_millionths() < after.score_millionths()
        || (hit.score_millionths() == after.score_millionths()
            && (hit.fact().telemetry().updated_at() < after.updated_at()
                || (hit.fact().telemetry().updated_at() == after.updated_at()
                    && hit.fact().fact_id() > after.fact_id())))
}

fn validate_lineage(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    after: Option<&tracedecay_store::FactLineageCursor>,
    limit: usize,
    events: &[FactLineageEventV1],
) -> Result<(), MemoryApplicationError> {
    if events.len() > limit
        || events
            .iter()
            .any(|event| event.owner() != owner || event.fact_id() != fact_id)
        || after.is_some_and(|after| {
            events.iter().any(|event| {
                (event.occurred_at(), event.event_id()) <= (after.occurred_at(), after.event_id())
            })
        })
        || events.windows(2).any(|pair| {
            (pair[0].occurred_at(), pair[0].event_id())
                >= (pair[1].occurred_at(), pair[1].event_id())
        })
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "fact lineage bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

fn validate_compatibility_projection(
    owner: &FactOwnerV1,
    target: &CompatibilityFactTargetV1,
    projection: &CompatibilityFactProjectionV1,
) -> Result<(), MemoryApplicationError> {
    if projection.owner() != owner {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility projection owner",
        });
    }
    if let Some(fact_id) = target.canonical_fact_id() {
        if projection.fact_id() != fact_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility projection canonical identity",
            });
        }
    } else if let (Some(query), CompatibilityFactProjectionV1::Available(fact)) =
        (target.legacy_query(), projection)
    {
        let mapping = fact.mapping().legacy_mapping();
        if mapping.is_none_or(|mapping| {
            mapping.owner() != owner
                || mapping.source_store_id() != query.source_store_id()
                || mapping.legacy_fact_id() != query.legacy_fact_id()
        }) {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility projection legacy mapping",
            });
        }
    }
    Ok(())
}

fn validate_compatibility_inspection(
    owner: &FactOwnerV1,
    target: &CompatibilityFactTargetV1,
    inspection: &CompatibilityFactInspectionV1,
) -> Result<(), MemoryApplicationError> {
    if inspection.owner() != owner
        || inspection.history().owner() != owner
        || inspection.status().owner() != owner
        || inspection.history().fact_id() != inspection.fact().fact_id()
        || inspection
            .status()
            .fact_id()
            .is_some_and(|fact_id| fact_id != inspection.fact().fact_id())
        || inspection
            .anchors()
            .iter()
            .any(|anchor| FactOwnerV1::from(anchor.owner().clone()) != *owner)
        || inspection
            .anchors()
            .windows(2)
            .any(|pair| pair[0].anchor_id() >= pair[1].anchor_id())
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility inspection owner and identity",
        });
    }
    match target {
        CompatibilityFactTargetV1::Canonical(target)
            if inspection.fact().fact_id() != target.fact_id() =>
        {
            Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility inspection canonical identity",
            })
        }
        CompatibilityFactTargetV1::Legacy(query) => {
            let mapping = inspection.fact().mapping().legacy_mapping();
            if mapping.is_none_or(|mapping| {
                mapping.owner() != owner
                    || mapping.source_store_id() != query.source_store_id()
                    || mapping.legacy_fact_id() != query.legacy_fact_id()
            }) {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "compatibility inspection legacy mapping",
                });
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_compatibility_add_outcome(
    owner: &FactOwnerV1,
    outcome: &CompatibilityFactAddOutcomeV1,
) -> Result<(), MemoryApplicationError> {
    if outcome
        .fact()
        .is_some_and(|projection| projection.owner() != owner)
        || outcome
            .closest_fact_id()
            .is_some_and(|fact_id| fact_id.owner() != owner)
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility add outcome owner",
        });
    }
    Ok(())
}

fn validate_compatibility_proposal(
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    proposal: &CompatibilityFactProposalRecordV1,
) -> Result<(), MemoryApplicationError> {
    if proposal.owner() != owner
        || proposal.proposal_id() != proposal_id
        || proposal.request().owner() != owner
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility proposal owner and identity",
        });
    }
    Ok(())
}

fn validate_compatibility_proposal_page(
    owner: &FactOwnerV1,
    after_proposal_id: Option<&ProvenanceId>,
    limit: usize,
    page: &CompatibilityFactProposalPageV1,
) -> Result<(), MemoryApplicationError> {
    let proposals = page.proposals();
    let cursor_is_invalid = page.next_after_proposal_id().is_some_and(|cursor| {
        cursor.validate().is_err()
            || after_proposal_id.is_some_and(|after| cursor <= after)
            || proposals
                .last()
                .is_none_or(|proposal| cursor <= proposal.proposal_id())
    });
    if page.owner() != owner
        || proposals.len() > limit
        || proposals.iter().any(|proposal| proposal.owner() != owner)
        || after_proposal_id.is_some_and(|after| {
            proposals
                .iter()
                .any(|proposal| proposal.proposal_id() <= after)
        })
        || proposals
            .windows(2)
            .any(|pair| pair[0].proposal_id() >= pair[1].proposal_id())
        || cursor_is_invalid
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "compatibility proposal page bounds, owner, cursor, and ordering",
        });
    }
    Ok(())
}

/// Explicit quarantine for the V1 mutable/i64 API. Implementations translate
/// compatibility DTOs into canonical batches or projections before invoking
/// [`MemoryApplication`]; they are never an authoritative persistence port.
pub mod legacy_compatibility {
    use std::error::Error as StdError;

    use thiserror::Error;
    use tracedecay_domain::{DomainError, FactOwnerV1};
    use tracedecay_store::{FactWriteBatch, StoredFactV1};

    use crate::memory::types::{AddFactRequest, FactRecord};

    #[derive(Debug, Error)]
    pub enum LegacyMemoryCompatibilityError {
        #[error("legacy memory owner is invalid")]
        InvalidOwner(#[source] DomainError),
        #[error("legacy conversion produced a batch for a different owner")]
        OwnerMismatch {
            expected: FactOwnerV1,
            actual: FactOwnerV1,
        },
        #[error("legacy memory compatibility conversion failed")]
        Adapter {
            #[source]
            source: Box<dyn StdError + Send + Sync>,
        },
    }

    /// The only V1 request conversion boundary. Implementations own canonical
    /// identity, sanitization, and deterministic batch assembly; callers only
    /// supply an immutable owner scope and a legacy request.
    pub trait LegacyMemoryCompatibilityAdapter {
        type Error: StdError + Send + Sync + 'static;

        fn add_request_to_batch(
            &self,
            request: AddFactRequest,
        ) -> Result<FactWriteBatch, Self::Error>;

        fn project_fact_record(&self, fact: &StoredFactV1) -> Result<FactRecord, Self::Error>;
    }

    pub fn prepare_add<A: LegacyMemoryCompatibilityAdapter>(
        owner: &FactOwnerV1,
        adapter: &A,
        request: AddFactRequest,
    ) -> Result<FactWriteBatch, LegacyMemoryCompatibilityError> {
        owner
            .validate()
            .map_err(LegacyMemoryCompatibilityError::InvalidOwner)?;
        let batch = adapter.add_request_to_batch(request).map_err(|source| {
            LegacyMemoryCompatibilityError::Adapter {
                source: Box::new(source),
            }
        })?;
        validate_owner_bound_batch(owner, batch)
    }

    fn validate_owner_bound_batch(
        owner: &FactOwnerV1,
        batch: FactWriteBatch,
    ) -> Result<FactWriteBatch, LegacyMemoryCompatibilityError> {
        if batch.owner() != owner {
            return Err(LegacyMemoryCompatibilityError::OwnerMismatch {
                expected: owner.clone(),
                actual: batch.owner().clone(),
            });
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
        Confidence, CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass,
        FactAssertionId, FactEventId, FactIdentityMaterialV1, FactIdentitySourceV1,
        FactLineageEventKindV1, ObservationScopeV1, PayloadAccessState,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
        ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2Parts,
        RetrievalAnchorTargetV2, ScopeResolutionId, SourceStoreId, UtcMicros, VectorWatermark,
    };
    use tracedecay_store::{FactCommitReceipt, FactLineageCursor, FactStoreResult};

    use super::*;
    use crate::memory::types::{AddFactRequest, FactRecord, MemoryCategory};

    #[derive(Default)]
    struct FakeAuthority {
        committed: Mutex<Vec<FactWriteBatch>>,
        next_commit_outcome: Mutex<Option<FactCommitOutcome>>,
        promotions: Mutex<Vec<PromoteFactProposal>>,
        promotion_conflict: Mutex<Option<Option<FactProposalPromotionStateV1>>>,
        current_queries: Mutex<Vec<CurrentFactsQuery>>,
        current_results: Mutex<Vec<StoredFactV1>>,
        current_fact_queries: Mutex<Vec<FactCurrentQuery>>,
        current_fact_result: Mutex<Option<StoredFactV1>>,
        as_of_queries: Mutex<Vec<FactAsOfQuery>>,
        as_of_result: Mutex<Option<StoredFactV1>>,
        lineage_queries: Mutex<Vec<FactLineageQuery>>,
        lineage_results: Mutex<Vec<FactLineageEventV1>>,
        legacy_queries: Mutex<Vec<LegacyFactQuery>>,
        legacy_result: Mutex<Option<FactId>>,
        anchor_queries: Mutex<Vec<RetrievalAnchorId>>,
        compatibility_calls: Mutex<Vec<&'static str>>,
    }

    #[derive(Default)]
    struct UnavailableEvidenceResolver {
        requests: Mutex<Vec<(FactOwnerV1, RetrievalAnchorId)>>,
    }

    impl EvidenceAnchorResolver for UnavailableEvidenceResolver {
        async fn resolve_evidence_anchor(
            &self,
            owner: FactOwnerV1,
            anchor_id: RetrievalAnchorId,
        ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
            self.requests
                .lock()
                .unwrap()
                .push((owner, anchor_id.clone()));
            Err(EvidenceAnchorResolutionError::Unavailable { anchor_id })
        }
    }

    struct StaticEvidenceResolver {
        record: ResolvedEvidenceAnchorV1,
    }

    impl EvidenceAnchorResolver for StaticEvidenceResolver {
        async fn resolve_evidence_anchor(
            &self,
            _owner: FactOwnerV1,
            _anchor_id: RetrievalAnchorId,
        ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
            Ok(self.record.clone())
        }
    }

    struct LegacyBatchAdapter {
        batch_owner: FactOwnerV1,
    }

    impl legacy_compatibility::LegacyMemoryCompatibilityAdapter for LegacyBatchAdapter {
        type Error = std::io::Error;

        fn add_request_to_batch(
            &self,
            _request: AddFactRequest,
        ) -> Result<FactWriteBatch, Self::Error> {
            Ok(batch(
                self.batch_owner.clone(),
                "operation.legacy-compatibility.add",
            ))
        }

        fn project_fact_record(&self, _fact: &StoredFactV1) -> Result<FactRecord, Self::Error> {
            Err(std::io::Error::other("not exercised by batch preparation"))
        }
    }

    struct FailingLegacyBatchAdapter;

    impl legacy_compatibility::LegacyMemoryCompatibilityAdapter for FailingLegacyBatchAdapter {
        type Error = std::io::Error;

        fn add_request_to_batch(
            &self,
            _request: AddFactRequest,
        ) -> Result<FactWriteBatch, Self::Error> {
            Err(std::io::Error::other("sanitization rejected fixture"))
        }

        fn project_fact_record(&self, _fact: &StoredFactV1) -> Result<FactRecord, Self::Error> {
            Err(std::io::Error::other("not exercised by batch preparation"))
        }
    }

    impl FactStore for FakeAuthority {
        async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
            let outcome = self
                .next_commit_outcome
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| committed_outcome(&batch));
            self.committed.lock().unwrap().push(batch);
            Ok(outcome)
        }

        async fn query_current_facts(
            &self,
            query: CurrentFactsQuery,
        ) -> FactStoreResult<Vec<StoredFactV1>> {
            self.current_queries.lock().unwrap().push(query);
            Ok(self.current_results.lock().unwrap().clone())
        }

        async fn query_fact_as_of(
            &self,
            query: FactAsOfQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.as_of_queries.lock().unwrap().push(query);
            Ok(self.as_of_result.lock().unwrap().clone())
        }

        async fn query_fact_current(
            &self,
            query: FactCurrentQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.current_fact_queries.lock().unwrap().push(query);
            Ok(self.current_fact_result.lock().unwrap().clone())
        }

        async fn query_fact_lineage(
            &self,
            query: FactLineageQuery,
        ) -> FactStoreResult<Vec<FactLineageEventV1>> {
            self.lineage_queries.lock().unwrap().push(query);
            Ok(self.lineage_results.lock().unwrap().clone())
        }

        async fn resolve_legacy_fact(
            &self,
            query: LegacyFactQuery,
        ) -> FactStoreResult<Option<FactId>> {
            self.legacy_queries.lock().unwrap().push(query);
            Ok(self.legacy_result.lock().unwrap().clone())
        }

        async fn get_retrieval_anchor(
            &self,
            query: RetrievalAnchorQuery,
        ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
            self.anchor_queries
                .lock()
                .unwrap()
                .push(query.anchor_id().clone());
            Ok(None)
        }
    }

    impl FactProposalStore for FakeAuthority {
        async fn promote_fact_proposal(
            &self,
            promotion: PromoteFactProposal,
        ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
            if let Some(actual) = self.promotion_conflict.lock().unwrap().take() {
                return Err(FactProposalStoreError::ProposalStateConflict {
                    proposal_id: promotion.proposal_id().clone(),
                    expected: promotion.expected_state(),
                    actual,
                });
            }
            let outcome = committed_outcome(promotion.batch());
            let result = PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                outcome,
            )
            .map_err(FactStoreError::from)?;
            self.promotions.lock().unwrap().push(promotion);
            Ok(result)
        }
    }

    impl FactCompatibilityStore for FakeAuthority {
        async fn execute_compatibility_read(
            &self,
            command: FactCompatibilityReadCommandV1,
        ) -> Result<FactCompatibilityReadOutcomeV1, FactCompatibilityStoreError> {
            self.compatibility_reads
                .lock()
                .unwrap()
                .push(command.clone());
            if let Some(outcome) = self.next_compatibility_read_outcome.lock().unwrap().take() {
                return Ok(outcome);
            }
            match command {
                FactCompatibilityReadCommandV1::List(query) => {
                    Ok(FactCompatibilityReadOutcomeV1::List(
                        CompatibilityFactPageV1::new(query.owner().clone(), vec![], None)?,
                    ))
                }
                FactCompatibilityReadCommandV1::Search(query) => {
                    Ok(FactCompatibilityReadOutcomeV1::Search(
                        CompatibilityFactSearchPageV1::new(query.owner().clone(), vec![], None)?,
                    ))
                }
                FactCompatibilityReadCommandV1::Get(_) => {
                    Ok(FactCompatibilityReadOutcomeV1::Get(None))
                }
                FactCompatibilityReadCommandV1::History(query) => Ok(
                    FactCompatibilityReadOutcomeV1::History(CompatibilityFactHistoryV1::new(
                        query.owner().clone(),
                        query.fact_id().clone(),
                        vec![],
                        None,
                    )?),
                ),
                FactCompatibilityReadCommandV1::Status(query) => Ok(
                    FactCompatibilityReadOutcomeV1::Status(CompatibilityFactStatusV1::new(
                        query.owner().clone(),
                        Some(query.fact_id().clone()),
                        None,
                        tracedecay_store::CompatibilityProjectionStateV1::Ready,
                        None,
                        None,
                    )?),
                ),
                FactCompatibilityReadCommandV1::Inspect(_) => {
                    Ok(FactCompatibilityReadOutcomeV1::Inspect(None))
                }
            }
        }

        async fn execute_compatibility_mutation(
            &self,
            command: FactCompatibilityMutationCommandV1,
        ) -> Result<FactCompatibilityMutationOutcomeV1, FactCompatibilityStoreError> {
            let FactCompatibilityMutationCommandV1::Commit(batch) = &command;
            let outcome = committed_outcome(batch);
            self.compatibility_mutations.lock().unwrap().push(command);
            Ok(FactCompatibilityMutationOutcomeV1::Commit(outcome))
        }

        async fn execute_compatibility_proposal(
            &self,
            command: FactCompatibilityProposalCommandV1,
        ) -> Result<FactCompatibilityProposalOutcomeV1, FactCompatibilityStoreError> {
            let FactCompatibilityProposalCommandV1::Promote(promotion) = &command;
            let outcome = PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                committed_outcome(promotion.batch()),
            )
            .map_err(FactStoreError::from)?;
            self.compatibility_proposals.lock().unwrap().push(command);
            Ok(FactCompatibilityProposalOutcomeV1::Promote(outcome))
        }
    }

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.memory.application").unwrap(),
        }
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner,
                FactIdentitySourceV1::Application {
                    operation_id: id(operation),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn batch(owner: FactOwnerV1, operation: &str) -> FactWriteBatch {
        let fact_id = fact_id(owner.clone(), operation);
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        FactWriteBatch::new(
            fact_id,
            owner,
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap()
    }

    fn committed_outcome(batch: &FactWriteBatch) -> FactCommitOutcome {
        let event_ids: Vec<FactEventId> = batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect();
        let last_event_id = event_ids.last().unwrap().clone();
        let active_assertion_id: Option<FactAssertionId> = batch
            .assertion()
            .map(|assertion| assertion.assertion_id().clone());
        FactCommitOutcome::Committed(
            FactCommitReceipt::new(
                batch.fact_id().clone(),
                batch.owner().clone(),
                event_ids,
                last_event_id,
                active_assertion_id,
            )
            .unwrap(),
        )
    }

    fn stored_fact(
        owner: FactOwnerV1,
        operation: &str,
        projected_as_of: UtcMicros,
    ) -> StoredFactV1 {
        let fact_id = fact_id(owner.clone(), operation);
        StoredFactV1::new(
            fact_id,
            owner,
            None,
            PayloadAccessState::Deleted,
            Confidence::new(0.5).unwrap(),
            id(&format!("assertion.{operation}")),
            id(&format!("event.{operation}")),
            None,
            projected_as_of,
        )
        .unwrap()
    }

    fn profile_anchor() -> RetrievalAnchorRecordV2 {
        const DIGEST_A: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const DIGEST_B: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::Entity(EntityRef {
                id: EntityId::new("entity.memory.external").unwrap(),
                kind: EntityKind::Document,
            }),
            owner: ObservationScopeV1::Profile,
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Unknown,
            projection_generation: ProjectionGenerationId::new("projection.memory.external")
                .unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors: vec![],
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.memory.external").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.memory.external").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
                capability_id: CapabilityId::new("capability.memory.external").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
            },
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.memory.external").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    fn legacy_add_request() -> AddFactRequest {
        AddFactRequest {
            content: "legacy conversion fixture".to_owned(),
            category: MemoryCategory::Project,
            source: None,
            tags: vec![],
            entities: vec![],
            trust: None,
            metadata: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn canonical_batch_is_the_single_write_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let write = batch(owner(), "operation.memory.commit");
        let expected_fact_id = write.fact_id().clone();

        let outcome = application.commit_fact(write).await.unwrap();

        assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
        let committed = application.authority.committed.lock().unwrap();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].fact_id(), &expected_fact_id);
    }

    #[tokio::test]
    async fn idempotent_replay_preserves_the_canonical_commit_identity() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let write = batch(owner(), "operation.memory.replay");
        let replay = match committed_outcome(&write) {
            FactCommitOutcome::Committed(receipt) => FactCommitOutcome::IdempotentReplay(receipt),
            _ => unreachable!("fixture always commits"),
        };
        *application.authority.next_commit_outcome.lock().unwrap() = Some(replay);

        let outcome = application.commit_fact(write).await.unwrap();

        assert!(matches!(outcome, FactCommitOutcome::IdempotentReplay(_)));
        assert_eq!(application.authority.committed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn evidence_resolution_is_owner_bound_at_the_daemon_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let resolver = UnavailableEvidenceResolver::default();
        let anchor_id = id::<RetrievalAnchorId>("anchor.memory.external");

        let error = application
            .resolve_evidence_anchor(&resolver, anchor_id.clone())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::EvidenceAnchor(EvidenceAnchorResolutionError::Unavailable {
                anchor_id: actual,
            }) if actual == anchor_id
        ));
        assert_eq!(
            resolver.requests.lock().unwrap().as_slice(),
            &[(owner(), anchor_id)]
        );
    }

    #[tokio::test]
    async fn evidence_resolution_rejects_a_cross_owner_daemon_reply() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let record = profile_anchor();
        let anchor_id = record.anchor_id().clone();
        let resolver = StaticEvidenceResolver {
            record: ResolvedEvidenceAnchorV1::new(record).unwrap(),
        };

        let error = application
            .resolve_evidence_anchor(&resolver, anchor_id)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner"
            }
        ));
    }

    #[test]
    fn legacy_prepare_add_preserves_the_explicit_owner_scope() {
        let adapter = LegacyBatchAdapter {
            batch_owner: owner(),
        };

        let add =
            legacy_compatibility::prepare_add(&owner(), &adapter, legacy_add_request()).unwrap();

        assert_eq!(add.owner(), &owner());
    }

    #[test]
    fn legacy_prepare_rejects_cross_owner_batches() {
        let adapter = LegacyBatchAdapter {
            batch_owner: FactOwnerV1::Profile,
        };

        let error = legacy_compatibility::prepare_add(&owner(), &adapter, legacy_add_request())
            .unwrap_err();

        assert!(matches!(
            error,
            legacy_compatibility::LegacyMemoryCompatibilityError::OwnerMismatch { .. }
        ));
    }

    #[test]
    fn legacy_prepare_preserves_typed_adapter_failures() {
        let error = legacy_compatibility::prepare_add(
            &owner(),
            &FailingLegacyBatchAdapter,
            legacy_add_request(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            legacy_compatibility::LegacyMemoryCompatibilityError::Adapter { .. }
        ));
    }

    #[tokio::test]
    async fn owner_mismatch_is_rejected_before_authority_access() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let error = application
            .commit_fact(batch(FactOwnerV1::Profile, "operation.profile.commit"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn query_owner_mismatch_is_rejected_before_authority_access() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let query = CurrentFactsQuery::new(FactOwnerV1::Profile, None, 10).unwrap();

        let error = application.query_current_facts(query).await.unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(
            application
                .authority
                .current_queries
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn compatibility_reads_use_typed_owner_bound_authority_commands() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.compatibility.read");
        let search = CompatibilityFactSearchQuery::new(
            owner(),
            tracedecay_store::CompatibilityFactSearchKindV1::Search,
            Some("compatibility fixture".to_owned()),
            None,
            10,
        )
        .unwrap();

        assert!(
            application
                .list_compatibility_facts(CurrentFactsQuery::new(owner(), None, 10).unwrap())
                .await
                .unwrap()
                .facts()
                .is_empty()
        );
        assert!(
            application
                .search_compatibility_facts(search)
                .await
                .unwrap()
                .hits()
                .is_empty()
        );
        assert!(
            application
                .get_compatibility_fact(FactCurrentQuery::new(owner(), fact_id.clone()).unwrap())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            application
                .get_compatibility_history(
                    FactLineageQuery::new(owner(), fact_id.clone(), None, 10).unwrap(),
                )
                .await
                .unwrap()
                .events()
                .is_empty()
        );
        assert_eq!(
            application
                .get_compatibility_status(FactCurrentQuery::new(owner(), fact_id.clone()).unwrap())
                .await
                .unwrap()
                .fact_id(),
            Some(&fact_id)
        );
        assert!(
            application
                .inspect_compatibility_fact(FactCurrentQuery::new(owner(), fact_id).unwrap())
                .await
                .unwrap()
                .is_none()
        );

        let commands = application.authority.compatibility_reads.lock().unwrap();
        assert!(matches!(
            commands.as_slice(),
            [
                FactCompatibilityReadCommandV1::List(_),
                FactCompatibilityReadCommandV1::Search(_),
                FactCompatibilityReadCommandV1::Get(_),
                FactCompatibilityReadCommandV1::History(_),
                FactCompatibilityReadCommandV1::Status(_),
                FactCompatibilityReadCommandV1::Inspect(_),
            ]
        ));
    }

    #[tokio::test]
    async fn compatibility_read_owner_mismatch_never_reaches_authority() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();

        let error = application
            .list_compatibility_facts(
                CurrentFactsQuery::new(FactOwnerV1::Profile, None, 10).unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::OwnerMismatch { .. }
        ));
        assert!(
            application
                .authority
                .compatibility_reads
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn compatibility_read_kind_and_cursor_are_authority_checked() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application
            .authority
            .next_compatibility_read_outcome
            .lock()
            .unwrap() = Some(FactCompatibilityReadOutcomeV1::Get(None));

        let error = application
            .list_compatibility_facts(CurrentFactsQuery::new(owner(), None, 10).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility list outcome kind"
            }
        ));

        *application
            .authority
            .next_compatibility_read_outcome
            .lock()
            .unwrap() = Some(FactCompatibilityReadOutcomeV1::List(
            CompatibilityFactPageV1::new(
                owner(),
                vec![],
                Some(fact_id(owner(), "operation.compatibility.invalid-cursor")),
            )
            .unwrap(),
        ));
        let error = application
            .list_compatibility_facts(CurrentFactsQuery::new(owner(), None, 10).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult {
                invariant: "compatibility list bounds, owner, cursor, and ordering"
            }
        ));
    }

    #[tokio::test]
    async fn compatibility_commit_and_promotion_remain_single_authority_commands() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let committed = application
            .commit_compatibility_fact(batch(owner(), "operation.compatibility.commit"))
            .await
            .unwrap();
        assert!(matches!(committed, FactCommitOutcome::Committed(_)));
        assert_eq!(
            application
                .authority
                .compatibility_mutations
                .lock()
                .unwrap()
                .len(),
            1
        );

        let promotion = PromoteFactProposal::new(
            id("proposal.compatibility.1"),
            owner(),
            FactProposalPromotionStateV1::PendingApproval,
            Some(id("actor.reviewer")),
            batch(owner(), "operation.compatibility.promote"),
        )
        .unwrap();
        let promoted = application
            .promote_compatibility_fact_proposal(promotion)
            .await
            .unwrap();
        assert!(matches!(promoted.commit(), FactCommitOutcome::Committed(_)));
        assert_eq!(
            application
                .authority
                .compatibility_proposals
                .lock()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn proposal_cas_and_batch_commit_are_one_authority_operation() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let promotion = PromoteFactProposal::new(
            id("proposal.memory.1"),
            owner(),
            FactProposalPromotionStateV1::PendingApproval,
            Some(id("actor.reviewer")),
            batch(owner(), "operation.proposal.promote"),
        )
        .unwrap();

        let outcome = application.promote_fact_proposal(promotion).await.unwrap();

        assert!(matches!(outcome.commit(), FactCommitOutcome::Committed(_)));
        assert_eq!(application.authority.promotions.lock().unwrap().len(), 1);
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn proposal_cas_conflict_is_typed_and_does_not_commit_a_batch() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application.authority.promotion_conflict.lock().unwrap() =
            Some(Some(FactProposalPromotionStateV1::Applying));
        let promotion = PromoteFactProposal::new(
            id("proposal.memory.conflict"),
            owner(),
            FactProposalPromotionStateV1::PendingApproval,
            Some(id("actor.reviewer")),
            batch(owner(), "operation.proposal.conflict"),
        )
        .unwrap();

        let error = application
            .promote_fact_proposal(promotion)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::Authority(FactProposalStoreError::ProposalStateConflict {
                expected: FactProposalPromotionStateV1::PendingApproval,
                actual: Some(FactProposalPromotionStateV1::Applying),
                ..
            })
        ));
        assert!(application.authority.promotions.lock().unwrap().is_empty());
        assert!(application.authority.committed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn typed_queries_propagate_without_identity_loss() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.memory.query");
        let current = CurrentFactsQuery::new(owner(), None, 10).unwrap();
        let current_fact = FactCurrentQuery::new(owner(), fact_id.clone()).unwrap();
        let as_of = FactAsOfQuery::new(owner(), fact_id.clone(), UtcMicros(5)).unwrap();
        let lineage = FactLineageQuery::new(owner(), fact_id, None, 10).unwrap();
        let legacy = LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap();
        let anchor_id = id::<RetrievalAnchorId>("anchor.memory.query");
        let anchor_query = RetrievalAnchorQuery::new(owner(), anchor_id.clone()).unwrap();

        assert!(
            application
                .query_current_facts(current)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            application
                .query_fact_current(current_fact)
                .await
                .unwrap()
                .is_none()
        );
        assert!(application.query_fact_as_of(as_of).await.unwrap().is_none());
        assert!(
            application
                .query_fact_lineage(lineage)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            application
                .resolve_legacy_fact(legacy)
                .await
                .unwrap()
                .is_none()
        );
        let anchor: Option<RetrievalAnchorRecordV2> = application
            .get_retrieval_anchor(anchor_query)
            .await
            .unwrap();
        assert!(anchor.is_none());

        assert_eq!(
            application.authority.current_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application
                .authority
                .current_fact_queries
                .lock()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(application.authority.as_of_queries.lock().unwrap().len(), 1);
        assert_eq!(
            application.authority.lineage_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application.authority.legacy_queries.lock().unwrap().len(),
            1
        );
        assert_eq!(
            application
                .authority
                .anchor_queries
                .lock()
                .unwrap()
                .as_slice(),
            &[anchor_id]
        );
    }

    #[tokio::test]
    async fn current_page_must_advance_cursor_and_stay_bounded() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let first = stored_fact(owner(), "operation.current.first", UtcMicros(1));
        *application.authority.current_results.lock().unwrap() = vec![first.clone()];

        let error = application
            .query_current_facts(
                CurrentFactsQuery::new(owner(), Some(first.fact_id().clone()), 1).unwrap(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));

        let second = stored_fact(owner(), "operation.current.second", UtcMicros(2));
        let mut results = vec![first, second];
        results.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
        *application.authority.current_results.lock().unwrap() = results;

        let error = application
            .query_current_facts(CurrentFactsQuery::new(owner(), None, 1).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn as_of_result_cannot_project_after_requested_time() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact = stored_fact(owner(), "operation.as-of.future", UtcMicros(6));
        *application.authority.as_of_result.lock().unwrap() = Some(fact.clone());

        let error = application
            .query_fact_as_of(
                FactAsOfQuery::new(owner(), fact.fact_id().clone(), UtcMicros(5)).unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn lineage_page_must_advance_cursor_and_stay_bounded() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        let fact_id = fact_id(owner(), "operation.lineage.cursor");
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(1),
            None,
        )
        .unwrap();
        let cursor = FactLineageCursor::new(event.occurred_at(), event.event_id().clone()).unwrap();
        *application.authority.lineage_results.lock().unwrap() = vec![event];

        let error = application
            .query_fact_lineage(FactLineageQuery::new(owner(), fact_id, Some(cursor), 1).unwrap())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[tokio::test]
    async fn legacy_resolution_cannot_cross_owner_boundary() {
        let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
        *application.authority.legacy_result.lock().unwrap() = Some(fact_id(
            FactOwnerV1::Profile,
            "operation.legacy.cross-owner",
        ));

        let error = application
            .resolve_legacy_fact(
                LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap(),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            MemoryApplicationError::InvalidAuthorityResult { .. }
        ));
    }

    #[test]
    fn stored_fact_fixture_remains_canonical() {
        let stored = stored_fact(owner(), "operation.memory.fixture", UtcMicros(2));
        let fact_id = stored.fact_id().clone();
        assert_eq!(stored.fact_id(), &fact_id);
    }
}
