//! Canonical memory use cases over the append-only fact authority.

use std::error::Error;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId,
    RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactStore, FactStoreError, FactWriteBatch, LegacyFactQuery, RetrievalAnchorQuery, StoredFactV1,
};

/// Authoritative proposal states from which an interrupted promotion may resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactProposalPromotionStateV1 {
    PendingApproval,
    Applying,
}

/// One compare-and-swap request whose proposal transition and fact batch must
/// commit in the same authority transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromoteFactProposal {
    proposal_id: ProvenanceId,
    owner: FactOwnerV1,
    expected_state: FactProposalPromotionStateV1,
    reviewer: Option<ActorId>,
    batch: FactWriteBatch,
}

impl PromoteFactProposal {
    pub fn new(
        proposal_id: ProvenanceId,
        owner: FactOwnerV1,
        expected_state: FactProposalPromotionStateV1,
        reviewer: Option<ActorId>,
        batch: FactWriteBatch,
    ) -> Result<Self, MemoryApplicationError> {
        proposal_id.validate()?;
        owner.validate()?;
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        if batch.owner() != &owner {
            let request_owner = batch.owner().clone();
            return Err(MemoryApplicationError::OwnerMismatch {
                scope: owner,
                request_owner,
            });
        }
        Ok(Self {
            proposal_id,
            owner,
            expected_state,
            reviewer,
            batch,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn expected_state(&self) -> FactProposalPromotionStateV1 {
        self.expected_state
    }

    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }

    pub fn batch(&self) -> &FactWriteBatch {
        &self.batch
    }

    pub fn into_batch(self) -> FactWriteBatch {
        self.batch
    }
}

/// Result of the authority transaction. A conflict outcome leaves the proposal
/// at `previous_state`; committed/replayed outcomes atomically promote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromoteFactProposalOutcome {
    proposal_id: ProvenanceId,
    previous_state: FactProposalPromotionStateV1,
    commit: FactCommitOutcome,
}

impl PromoteFactProposalOutcome {
    pub fn new(
        proposal_id: ProvenanceId,
        previous_state: FactProposalPromotionStateV1,
        commit: FactCommitOutcome,
    ) -> Result<Self, DomainError> {
        proposal_id.validate()?;
        Ok(Self {
            proposal_id,
            previous_state,
            commit,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }

    pub fn previous_state(&self) -> FactProposalPromotionStateV1 {
        self.previous_state
    }

    pub fn commit(&self) -> &FactCommitOutcome {
        &self.commit
    }
}

#[derive(Debug, Error)]
pub enum MemoryAuthorityError {
    #[error("fact authority operation failed")]
    Store(#[from] FactStoreError),
    #[error("fact proposal {proposal_id} state changed before promotion")]
    ProposalStateConflict {
        proposal_id: ProvenanceId,
        expected: FactProposalPromotionStateV1,
        actual: Option<FactProposalPromotionStateV1>,
    },
    #[error("memory authority operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

/// The canonical fact store plus the one compound authority operation that is
/// intentionally wider than `FactStore::commit_fact`.
pub trait MemoryAuthorityPort: FactStore {
    fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> impl Future<Output = Result<PromoteFactProposalOutcome, MemoryAuthorityError>> + Send;
}

#[derive(Debug, Error)]
pub enum MemoryApplicationError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("memory request owner does not match the application scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        request_owner: FactOwnerV1,
    },
    #[error("fact store operation failed")]
    Store(#[from] FactStoreError),
    #[error("memory authority operation failed")]
    Authority(#[from] MemoryAuthorityError),
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
}

/// Owner-bound application service. Paths, connections, legacy integer IDs,
/// and transport payloads never enter this boundary.
pub struct MemoryApplication<A> {
    owner: FactOwnerV1,
    authority: A,
}

impl<A: MemoryAuthorityPort> MemoryApplication<A> {
    pub fn new(owner: FactOwnerV1, authority: A) -> Result<Self, MemoryApplicationError> {
        owner.validate()?;
        Ok(Self { owner, authority })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
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

    pub async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> Result<Vec<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let facts = self.authority.query_current_facts(query).await?;
        validate_current_facts(&self.owner, &facts)?;
        Ok(facts)
    }

    pub async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        let fact_id = query.fact_id().clone();
        let fact = self.authority.query_fact_as_of(query).await?;
        if let Some(fact) = &fact
            && (fact.owner() != &self.owner || fact.fact_id() != &fact_id)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "as-of fact identity",
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
        let events = self.authority.query_fact_lineage(query).await?;
        validate_lineage(&self.owner, &fact_id, &events)?;
        Ok(events)
    }

    pub async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> Result<Option<FactId>, MemoryApplicationError> {
        self.ensure_owner(query.owner())?;
        Ok(self.authority.resolve_legacy_fact(query).await?)
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
    facts: &[StoredFactV1],
) -> Result<(), MemoryApplicationError> {
    if facts.iter().any(|fact| fact.owner() != owner)
        || facts
            .windows(2)
            .any(|pair| pair[0].fact_id() >= pair[1].fact_id())
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "current fact owner and ordering",
        });
    }
    Ok(())
}

fn validate_lineage(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    events: &[FactLineageEventV1],
) -> Result<(), MemoryApplicationError> {
    if events
        .iter()
        .any(|event| event.owner() != owner || event.fact_id() != fact_id)
        || events.windows(2).any(|pair| {
            (pair[0].occurred_at(), pair[0].event_id())
                >= (pair[1].occurred_at(), pair[1].event_id())
        })
    {
        return Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "fact lineage owner and ordering",
        });
    }
    Ok(())
}

/// Explicit quarantine for the V1 mutable/i64 API. Implementations translate
/// compatibility DTOs into canonical batches or projections before invoking
/// [`MemoryApplication`]; they are never an authoritative persistence port.
pub mod legacy_compatibility {
    use tracedecay_store::{FactWriteBatch, StoredFactV1};

    use crate::memory::types::{AddFactRequest, FactRecord, UpdateFactRequest};

    pub trait LegacyMemoryCompatibilityAdapter {
        type Error;

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
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tracedecay_domain::{
        Confidence, FactAssertionId, FactEventId, FactIdentityMaterialV1, FactIdentitySourceV1,
        FactLineageEventKindV1, PayloadAccessState, ProjectId, RetrievalAnchorId, SourceStoreId,
        UtcMicros,
    };
    use tracedecay_store::{FactCommitReceipt, FactStoreResult};

    use super::*;

    #[derive(Default)]
    struct FakeAuthority {
        committed: Mutex<Vec<FactWriteBatch>>,
        promotions: Mutex<Vec<PromoteFactProposal>>,
        current_queries: Mutex<Vec<CurrentFactsQuery>>,
        current_fact_queries: Mutex<Vec<FactCurrentQuery>>,
        as_of_queries: Mutex<Vec<FactAsOfQuery>>,
        lineage_queries: Mutex<Vec<FactLineageQuery>>,
        legacy_queries: Mutex<Vec<LegacyFactQuery>>,
        anchor_queries: Mutex<Vec<RetrievalAnchorId>>,
    }

    impl FactStore for FakeAuthority {
        async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
            let outcome = committed_outcome(&batch);
            self.committed.lock().unwrap().push(batch);
            Ok(outcome)
        }

        async fn query_current_facts(
            &self,
            query: CurrentFactsQuery,
        ) -> FactStoreResult<Vec<StoredFactV1>> {
            self.current_queries.lock().unwrap().push(query);
            Ok(Vec::new())
        }

        async fn query_fact_as_of(
            &self,
            query: FactAsOfQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.as_of_queries.lock().unwrap().push(query);
            Ok(None)
        }

        async fn query_fact_current(
            &self,
            query: FactCurrentQuery,
        ) -> FactStoreResult<Option<StoredFactV1>> {
            self.current_fact_queries.lock().unwrap().push(query);
            Ok(None)
        }

        async fn query_fact_lineage(
            &self,
            query: FactLineageQuery,
        ) -> FactStoreResult<Vec<FactLineageEventV1>> {
            self.lineage_queries.lock().unwrap().push(query);
            Ok(Vec::new())
        }

        async fn resolve_legacy_fact(
            &self,
            query: LegacyFactQuery,
        ) -> FactStoreResult<Option<FactId>> {
            self.legacy_queries.lock().unwrap().push(query);
            Ok(None)
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

    impl MemoryAuthorityPort for FakeAuthority {
        async fn promote_fact_proposal(
            &self,
            promotion: PromoteFactProposal,
        ) -> Result<PromoteFactProposalOutcome, MemoryAuthorityError> {
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

    #[test]
    fn stored_fact_fixture_remains_canonical() {
        let fact_id = fact_id(owner(), "operation.memory.fixture");
        let stored = StoredFactV1::new(
            fact_id.clone(),
            owner(),
            None,
            PayloadAccessState::Deleted,
            Confidence::new(0.5).unwrap(),
            id("assertion.fixture"),
            id("event.fixture"),
            None,
            UtcMicros(2),
        )
        .unwrap();
        assert_eq!(stored.fact_id(), &fact_id);
    }
}
