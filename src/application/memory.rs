//! Canonical memory use cases over the append-only fact authority.

use std::error::Error as StdError;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::{
    DomainError, FactId, FactLineageEventV1, FactOwnerV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactProposalPromotionStateV1, FactProposalStore, FactProposalStoreError, FactStore,
    FactStoreError, FactWriteBatch, LegacyFactQuery, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
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

/// Explicit quarantine for the V1 mutable/i64 API. Implementations translate
/// compatibility DTOs into canonical batches or projections before invoking
/// [`MemoryApplication`]; they are never an authoritative persistence port.
pub mod legacy_compatibility {
    use std::error::Error as StdError;

    use thiserror::Error;
    use tracedecay_domain::{DomainError, FactOwnerV1};
    use tracedecay_store::{FactWriteBatch, StoredFactV1};

    use crate::memory::types::{AddFactRequest, FactRecord, UpdateFactRequest};

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

        fn update_request_to_correction_batch(
            &self,
            request: UpdateFactRequest,
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

    pub fn prepare_update<A: LegacyMemoryCompatibilityAdapter>(
        owner: &FactOwnerV1,
        adapter: &A,
        request: UpdateFactRequest,
    ) -> Result<FactWriteBatch, LegacyMemoryCompatibilityError> {
        owner
            .validate()
            .map_err(LegacyMemoryCompatibilityError::InvalidOwner)?;
        let batch = adapter
            .update_request_to_correction_batch(request)
            .map_err(|source| LegacyMemoryCompatibilityError::Adapter {
                source: Box::new(source),
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
        Confidence, FactAssertionId, FactEventId, FactIdentityMaterialV1, FactIdentitySourceV1,
        FactLineageEventKindV1, PayloadAccessState, ProjectId, RetrievalAnchorId, SourceStoreId,
        UtcMicros,
    };
    use tracedecay_store::{FactCommitReceipt, FactLineageCursor, FactStoreResult};

    use super::*;

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
