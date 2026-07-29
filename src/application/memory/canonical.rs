//! Root store adapters for canonical memory application use cases.

use tracedecay_application::memory::{
    CommitFactCommandV1, CommitFactDispositionV1, CommitFactPort, CommitFactPortResultV1,
    CurrentFactsPort, CurrentFactsPortResultV1, CurrentFactsQueryV1, FactAsOfPort, FactAsOfQueryV1,
    FactCurrentPort, FactCurrentQueryV1, FactLineageCursorV1, FactLineagePort,
    FactLineagePortResultV1, FactLineageQueryV1, LegacyFactPort, LegacyFactQueryV1,
    MemoryApplication as CanonicalMemoryApplication, MemoryApplicationInvariantError,
    MemoryContradictionStateV1, MemoryFactSnapshotV1, MemoryReadCoverageV1, MemoryReadResultV1,
    MemoryUseCaseError, OptionalFactPortResultV1, PromoteFactProposalCommandV1,
    PromoteFactProposalPort, PromoteFactProposalPortResultV1, RetrievalAnchorPort,
    RetrievalAnchorQueryV1,
};
use tracedecay_domain::{FactId, FactLineageEventV1, FactOwnerV1, RetrievalAnchorRecordV2};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome,
    FactContradictionStateV1 as StoreFactContradictionStateV1, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactProposalPromotionStateV1, FactProposalStore,
    FactProposalStoreError, FactQueryCoverageV1, FactStore, FactStoreError, FactWriteBatch,
    LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery,
    StoredFactV1,
};

use super::{MemoryApplication, MemoryApplicationError};

struct FactStoreAdapter<'a, A>(&'a A);

impl<A: FactStore> CommitFactPort for FactStoreAdapter<'_, A> {
    type Command = FactWriteBatch;
    type Error = FactStoreError;
    type Output = FactCommitOutcome;

    async fn commit_fact(
        &self,
        command: Self::Command,
    ) -> Result<CommitFactPortResultV1<Self::Output>, Self::Error> {
        let outcome = self.0.commit_fact(command).await?;
        let (disposition, owner, fact_id) = commit_proof(&outcome);
        Ok(CommitFactPortResultV1::new(
            outcome,
            disposition,
            owner,
            fact_id,
        ))
    }
}

impl<A: FactStore> CurrentFactsPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = Vec<StoredFactV1>;
    type Query = CurrentFactsQuery;

    async fn query_current_facts(
        &self,
        query: Self::Query,
    ) -> Result<CurrentFactsPortResultV1<Self::Output>, Self::Error> {
        let facts = self.0.query_current_facts(query).await?;
        let snapshots = facts.iter().map(fact_snapshot).collect();
        Ok(CurrentFactsPortResultV1::new(facts, snapshots))
    }
}

impl<A: FactStore> FactAsOfPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResultV1<Option<StoredFactV1>>;
    type Query = FactAsOfQuery;

    async fn query_fact_as_of(
        &self,
        query: Self::Query,
    ) -> Result<OptionalFactPortResultV1<Self::Output>, Self::Error> {
        let response = self.0.query_fact_as_of_response(query).await?;
        let fact = response.fact().cloned();
        let snapshot = fact.as_ref().map(fact_snapshot);
        Ok(OptionalFactPortResultV1::new(
            as_of_read_result(&response, fact),
            snapshot,
        ))
    }
}

impl<A: FactStore> FactCurrentPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResultV1<Option<StoredFactV1>>;
    type Query = FactCurrentQuery;

    async fn query_fact_current(
        &self,
        query: Self::Query,
    ) -> Result<OptionalFactPortResultV1<Self::Output>, Self::Error> {
        let response = self.0.query_fact_current_response(query).await?;
        let fact = response.fact().cloned();
        let snapshot = fact.as_ref().map(fact_snapshot);
        Ok(OptionalFactPortResultV1::new(
            current_read_result(&response, fact),
            snapshot,
        ))
    }
}

impl<A: FactStore> FactLineagePort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResultV1<Vec<FactLineageEventV1>>;
    type Query = FactLineageQuery;

    async fn query_fact_lineage(
        &self,
        query: Self::Query,
    ) -> Result<FactLineagePortResultV1<Self::Output>, Self::Error> {
        let response = self.0.query_fact_lineage_response(query).await?;
        let events = response.events().to_vec();
        let output = MemoryReadResultV1::new(
            events.clone(),
            read_coverage(response.coverage()),
            contradiction_state(response.contradiction()),
        );
        Ok(FactLineagePortResultV1::new(output, events))
    }
}

impl<A: FactStore> LegacyFactPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Query = LegacyFactQuery;

    async fn resolve_legacy_fact(&self, query: Self::Query) -> Result<Option<FactId>, Self::Error> {
        self.0.resolve_legacy_fact(query).await
    }
}

impl<A: FactStore> RetrievalAnchorPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Query = RetrievalAnchorQuery;

    async fn get_retrieval_anchor(
        &self,
        query: Self::Query,
    ) -> Result<Option<RetrievalAnchorRecordV2>, Self::Error> {
        self.0.get_retrieval_anchor(query).await
    }
}

impl<A: FactProposalStore> PromoteFactProposalPort for FactStoreAdapter<'_, A> {
    type Command = PromoteFactProposal;
    type Error = FactProposalStoreError;
    type Output = PromoteFactProposalOutcome;
    type State = FactProposalPromotionStateV1;

    async fn promote_fact_proposal(
        &self,
        command: Self::Command,
    ) -> Result<PromoteFactProposalPortResultV1<Self::Output, Self::State>, Self::Error> {
        let outcome = self.0.promote_fact_proposal(command).await?;
        let (disposition, owner, fact_id) = commit_proof(outcome.commit());
        let proposal_id = outcome.proposal_id().clone();
        let previous_state = outcome.previous_state();
        Ok(PromoteFactProposalPortResultV1::new(
            outcome,
            proposal_id,
            previous_state,
            disposition,
            owner,
            fact_id,
        ))
    }
}

impl<A: FactStore> MemoryApplication<A> {
    pub async fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> Result<FactCommitOutcome, MemoryApplicationError> {
        let owner = batch.owner().clone();
        let fact_id = batch.fact_id().clone();
        canonical_application(&self.owner, &self.authority)?
            .commit_fact(CommitFactCommandV1::new(owner, fact_id, batch))
            .await
            .map_err(store_error)
    }

    pub async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> Result<Vec<StoredFactV1>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let after_fact_id = query.after_fact_id().cloned();
        let limit = query.limit();
        canonical_application(&self.owner, &self.authority)?
            .query_current_facts(CurrentFactsQueryV1::new(owner, after_fact_id, limit, query))
            .await
            .map_err(store_error)
    }

    pub async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let fact_id = query.fact_id().clone();
        let as_of = query.as_of();
        let result = canonical_application(&self.owner, &self.authority)?
            .query_fact_as_of(FactAsOfQueryV1::new(owner, fact_id, as_of, query))
            .await
            .map_err(store_error)?;
        Ok(result.into_payload())
    }

    pub async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> Result<Option<StoredFactV1>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let fact_id = query.fact_id().clone();
        let result = canonical_application(&self.owner, &self.authority)?
            .query_fact_current(FactCurrentQueryV1::new(owner, fact_id, query))
            .await
            .map_err(store_error)?;
        Ok(result.into_payload())
    }

    pub async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> Result<Vec<FactLineageEventV1>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let fact_id = query.fact_id().clone();
        let after = query.after().map(|cursor| {
            FactLineageCursorV1::new(cursor.occurred_at(), cursor.event_id().clone())
        });
        let limit = query.limit();
        let result = canonical_application(&self.owner, &self.authority)?
            .query_fact_lineage(FactLineageQueryV1::new(owner, fact_id, after, limit, query))
            .await
            .map_err(store_error)?;
        Ok(result.into_payload())
    }

    pub async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> Result<Option<FactId>, MemoryApplicationError> {
        let owner = query.owner().clone();
        canonical_application(&self.owner, &self.authority)?
            .resolve_legacy_fact(LegacyFactQueryV1::new(owner, query))
            .await
            .map_err(store_error)
    }

    pub async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let anchor_id = query.anchor_id().clone();
        canonical_application(&self.owner, &self.authority)?
            .get_retrieval_anchor(RetrievalAnchorQueryV1::new(owner, anchor_id, query))
            .await
            .map_err(store_error)
    }
}

impl<A: FactProposalStore> MemoryApplication<A> {
    pub async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, MemoryApplicationError> {
        let owner = promotion.owner().clone();
        let proposal_id = promotion.proposal_id().clone();
        let expected_state = promotion.expected_state();
        let fact_id = promotion.batch().fact_id().clone();
        canonical_application(&self.owner, &self.authority)?
            .promote_fact_proposal(PromoteFactProposalCommandV1::new(
                owner,
                proposal_id,
                expected_state,
                fact_id,
                promotion,
            ))
            .await
            .map_err(proposal_error)
    }
}

fn canonical_application<'a, A>(
    owner: &FactOwnerV1,
    authority: &'a A,
) -> Result<CanonicalMemoryApplication<FactStoreAdapter<'a, A>>, MemoryApplicationError> {
    CanonicalMemoryApplication::new(owner.clone(), FactStoreAdapter(authority))
        .map_err(invariant_error)
}

fn fact_snapshot(fact: &StoredFactV1) -> MemoryFactSnapshotV1 {
    MemoryFactSnapshotV1::new(
        fact.owner().clone(),
        fact.fact_id().clone(),
        fact.projected_as_of(),
    )
}

fn as_of_read_result(
    response: &FactAsOfResponseV1,
    fact: Option<StoredFactV1>,
) -> MemoryReadResultV1<Option<StoredFactV1>> {
    MemoryReadResultV1::new(
        fact,
        read_coverage(response.coverage()),
        contradiction_state(response.contradiction()),
    )
}

fn current_read_result(
    response: &FactCurrentResponseV1,
    fact: Option<StoredFactV1>,
) -> MemoryReadResultV1<Option<StoredFactV1>> {
    MemoryReadResultV1::new(
        fact,
        read_coverage(response.coverage()),
        contradiction_state(response.contradiction()),
    )
}

const fn read_coverage(coverage: &FactQueryCoverageV1) -> MemoryReadCoverageV1 {
    MemoryReadCoverageV1::new(
        coverage.visible(),
        coverage.hidden(),
        coverage.unknown(),
        coverage.redacted(),
    )
}

fn contradiction_state(
    contradiction: &StoreFactContradictionStateV1,
) -> MemoryContradictionStateV1 {
    match contradiction {
        StoreFactContradictionStateV1::Unknown => MemoryContradictionStateV1::Unknown,
        StoreFactContradictionStateV1::NotObserved => MemoryContradictionStateV1::NotObserved,
        StoreFactContradictionStateV1::Present { contradicted_by } => {
            MemoryContradictionStateV1::Present {
                contradicted_by: contradicted_by.clone(),
            }
        }
    }
}

fn commit_proof(
    outcome: &FactCommitOutcome,
) -> (CommitFactDispositionV1, Option<FactOwnerV1>, Option<FactId>) {
    match outcome {
        FactCommitOutcome::Committed(receipt) => (
            CommitFactDispositionV1::Committed,
            Some(receipt.owner().clone()),
            Some(receipt.fact_id().clone()),
        ),
        FactCommitOutcome::IdempotentReplay(receipt) => (
            CommitFactDispositionV1::IdempotentReplay,
            Some(receipt.owner().clone()),
            Some(receipt.fact_id().clone()),
        ),
        FactCommitOutcome::Conflict(_) => (CommitFactDispositionV1::Conflict, None, None),
        _ => (CommitFactDispositionV1::Unrecognized, None, None),
    }
}

fn store_error(error: MemoryUseCaseError<FactStoreError>) -> MemoryApplicationError {
    match error {
        MemoryUseCaseError::Invariant(error) => invariant_error(error),
        MemoryUseCaseError::Authority(error) => MemoryApplicationError::Store(error),
    }
}

fn proposal_error(error: MemoryUseCaseError<FactProposalStoreError>) -> MemoryApplicationError {
    match error {
        MemoryUseCaseError::Invariant(error) => invariant_error(error),
        MemoryUseCaseError::Authority(error) => MemoryApplicationError::Authority(error),
    }
}

fn invariant_error(error: MemoryApplicationInvariantError) -> MemoryApplicationError {
    match error {
        MemoryApplicationInvariantError::InvalidOwner(error) => {
            MemoryApplicationError::InvalidOwner(error)
        }
        MemoryApplicationInvariantError::OwnerMismatch {
            scope,
            request_owner,
        } => MemoryApplicationError::OwnerMismatch {
            scope,
            request_owner,
        },
        MemoryApplicationInvariantError::InvalidAuthorityResult { invariant } => {
            MemoryApplicationError::InvalidAuthorityResult { invariant }
        }
    }
}
