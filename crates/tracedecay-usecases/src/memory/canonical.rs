//! Root store adapters for canonical memory application use cases.

use tracedecay_application::memory::{
    CommitFactPort, CurrentFactsPort, FactAsOfPort, FactCurrentPort, FactLineagePort,
    MemoryApplication as CanonicalMemoryApplication, MemoryApplicationInvariantError,
    MemoryCommitFactCommand, MemoryCommitFactDisposition, MemoryCommitFactPortResult,
    MemoryContradictionState, MemoryCurrentFactsPortResult, MemoryCurrentFactsQuery,
    MemoryFactAsOfQuery, MemoryFactCurrentQuery, MemoryFactLineageCursor,
    MemoryFactLineagePortResult, MemoryFactLineageQuery, MemoryFactSnapshot,
    MemoryOptionalFactPortResult, MemoryReadCoverage, MemoryReadResult, MemoryRetrievalAnchorQuery,
    MemoryUseCaseError, RetrievalAnchorPort,
};
use tracedecay_domain::{FactId, FactLineageEventV1, FactOwnerV1, RetrievalAnchorRecordV2};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome,
    FactContradictionStateV1 as StoreFactContradictionStateV1, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactQueryCoverageV1, FactStore, FactStoreError,
    FactWriteBatch, FactWriteControl, RetrievalAnchorQuery, StoredFactV1,
};

use super::{MemoryApplication, MemoryApplicationError};

struct FactStoreAdapter<'a, A>(&'a A);

struct FactStoreWriteAdapter<'a, A> {
    authority: &'a A,
    write_control: &'a FactWriteControl,
}

impl<A: FactStore> CommitFactPort for FactStoreWriteAdapter<'_, A> {
    type Command = FactWriteBatch;
    type Error = FactStoreError;
    type Output = FactCommitOutcome;

    async fn commit_fact(
        &self,
        command: Self::Command,
    ) -> Result<MemoryCommitFactPortResult<Self::Output>, Self::Error> {
        let outcome = self
            .authority
            .commit_fact(command, self.write_control)
            .await?;
        let (disposition, owner, fact_id) = commit_proof(&outcome);
        Ok(MemoryCommitFactPortResult::new(
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
    ) -> Result<MemoryCurrentFactsPortResult<Self::Output>, Self::Error> {
        let facts = self.0.query_current_facts(query).await?;
        let snapshots = facts.iter().map(fact_snapshot).collect();
        Ok(MemoryCurrentFactsPortResult::new(facts, snapshots))
    }
}

impl<A: FactStore> FactAsOfPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResult<Option<StoredFactV1>>;
    type Query = FactAsOfQuery;

    async fn query_fact_as_of(
        &self,
        query: Self::Query,
    ) -> Result<MemoryOptionalFactPortResult<Self::Output>, Self::Error> {
        let response = self.0.query_fact_as_of_response(query).await?;
        let fact = response.fact().cloned();
        let snapshot = fact.as_ref().map(fact_snapshot);
        Ok(MemoryOptionalFactPortResult::new(
            as_of_read_result(&response, fact),
            snapshot,
        ))
    }
}

impl<A: FactStore> FactCurrentPort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResult<Option<StoredFactV1>>;
    type Query = FactCurrentQuery;

    async fn query_fact_current(
        &self,
        query: Self::Query,
    ) -> Result<MemoryOptionalFactPortResult<Self::Output>, Self::Error> {
        let response = self.0.query_fact_current_response(query).await?;
        let fact = response.fact().cloned();
        let snapshot = fact.as_ref().map(fact_snapshot);
        Ok(MemoryOptionalFactPortResult::new(
            current_read_result(&response, fact),
            snapshot,
        ))
    }
}

impl<A: FactStore> FactLineagePort for FactStoreAdapter<'_, A> {
    type Error = FactStoreError;
    type Output = MemoryReadResult<Vec<FactLineageEventV1>>;
    type Query = FactLineageQuery;

    async fn query_fact_lineage(
        &self,
        query: Self::Query,
    ) -> Result<MemoryFactLineagePortResult<Self::Output>, Self::Error> {
        let response = self.0.query_fact_lineage_response(query).await?;
        let events = response.events().to_vec();
        let output = MemoryReadResult::new(
            events.clone(),
            read_coverage(response.coverage()),
            contradiction_state(response.contradiction()),
        );
        Ok(MemoryFactLineagePortResult::new(output, events))
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

impl<A: FactStore> MemoryApplication<A> {
    pub async fn commit_fact(
        &self,
        batch: FactWriteBatch,
        write_control: &FactWriteControl,
    ) -> Result<FactCommitOutcome, MemoryApplicationError> {
        let owner = batch.owner().clone();
        let fact_id = batch.fact_id().clone();
        canonical_write_application(&self.owner, &self.authority, write_control)?
            .commit_fact(MemoryCommitFactCommand::new(owner, fact_id, batch))
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
            .query_current_facts(MemoryCurrentFactsQuery::new(
                owner,
                after_fact_id,
                limit,
                query,
            ))
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
            .query_fact_as_of(MemoryFactAsOfQuery::new(owner, fact_id, as_of, query))
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
            .query_fact_current(MemoryFactCurrentQuery::new(owner, fact_id, query))
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
            MemoryFactLineageCursor::new(cursor.occurred_at(), cursor.event_id().clone())
        });
        let limit = query.limit();
        let result = canonical_application(&self.owner, &self.authority)?
            .query_fact_lineage(MemoryFactLineageQuery::new(
                owner, fact_id, after, limit, query,
            ))
            .await
            .map_err(store_error)?;
        Ok(result.into_payload())
    }

    pub async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryApplicationError> {
        let owner = query.owner().clone();
        let anchor_id = query.anchor_id().clone();
        canonical_application(&self.owner, &self.authority)?
            .get_retrieval_anchor(MemoryRetrievalAnchorQuery::new(owner, anchor_id, query))
            .await
            .map_err(store_error)
    }
}

fn canonical_application<'a, A>(
    owner: &FactOwnerV1,
    authority: &'a A,
) -> Result<CanonicalMemoryApplication<FactStoreAdapter<'a, A>>, MemoryApplicationError> {
    CanonicalMemoryApplication::new(owner.clone(), FactStoreAdapter(authority))
        .map_err(invariant_error)
}

fn canonical_write_application<'a, A>(
    owner: &FactOwnerV1,
    authority: &'a A,
    write_control: &'a FactWriteControl,
) -> Result<CanonicalMemoryApplication<FactStoreWriteAdapter<'a, A>>, MemoryApplicationError> {
    CanonicalMemoryApplication::new(
        owner.clone(),
        FactStoreWriteAdapter {
            authority,
            write_control,
        },
    )
    .map_err(invariant_error)
}

fn fact_snapshot(fact: &StoredFactV1) -> MemoryFactSnapshot {
    MemoryFactSnapshot::new(
        fact.owner().clone(),
        fact.fact_id().clone(),
        fact.projected_as_of(),
    )
}

fn as_of_read_result(
    response: &FactAsOfResponseV1,
    fact: Option<StoredFactV1>,
) -> MemoryReadResult<Option<StoredFactV1>> {
    MemoryReadResult::new(
        fact,
        read_coverage(response.coverage()),
        contradiction_state(response.contradiction()),
    )
}

fn current_read_result(
    response: &FactCurrentResponseV1,
    fact: Option<StoredFactV1>,
) -> MemoryReadResult<Option<StoredFactV1>> {
    MemoryReadResult::new(
        fact,
        read_coverage(response.coverage()),
        contradiction_state(response.contradiction()),
    )
}

const fn read_coverage(coverage: &FactQueryCoverageV1) -> MemoryReadCoverage {
    MemoryReadCoverage::new(
        coverage.visible(),
        coverage.hidden(),
        coverage.unknown(),
        coverage.redacted(),
    )
}

fn contradiction_state(contradiction: &StoreFactContradictionStateV1) -> MemoryContradictionState {
    match contradiction {
        StoreFactContradictionStateV1::Unknown => MemoryContradictionState::Unknown,
        StoreFactContradictionStateV1::NotObserved => MemoryContradictionState::NotObserved,
        StoreFactContradictionStateV1::Present { contradicted_by } => {
            MemoryContradictionState::Present {
                contradicted_by: contradicted_by.clone(),
            }
        }
    }
}

fn commit_proof(
    outcome: &FactCommitOutcome,
) -> (
    MemoryCommitFactDisposition,
    Option<FactOwnerV1>,
    Option<FactId>,
) {
    match outcome {
        FactCommitOutcome::Committed(receipt) => (
            MemoryCommitFactDisposition::Committed,
            Some(receipt.owner().clone()),
            Some(receipt.fact_id().clone()),
        ),
        FactCommitOutcome::IdempotentReplay(receipt) => (
            MemoryCommitFactDisposition::IdempotentReplay,
            Some(receipt.owner().clone()),
            Some(receipt.fact_id().clone()),
        ),
        FactCommitOutcome::Conflict(_) => (MemoryCommitFactDisposition::Conflict, None, None),
        _ => (MemoryCommitFactDisposition::Unrecognized, None, None),
    }
}

fn store_error(error: MemoryUseCaseError<FactStoreError>) -> MemoryApplicationError {
    match error {
        MemoryUseCaseError::Invariant(error) => invariant_error(error),
        MemoryUseCaseError::Authority(error) => MemoryApplicationError::Store(error),
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
