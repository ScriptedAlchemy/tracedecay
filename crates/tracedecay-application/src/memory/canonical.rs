//! Owner-bound canonical memory use cases over transport-neutral ports.

use std::fmt::Debug;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::{
    DomainError, FactEventId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId,
    RetrievalAnchorId, RetrievalAnchorRecordV2, UtcMicros,
};

#[derive(Debug, Error)]
pub enum MemoryApplicationInvariantError {
    #[error("memory owner is invalid")]
    InvalidOwner(#[from] DomainError),
    #[error("memory request owner does not match the application scope")]
    OwnerMismatch {
        scope: FactOwnerV1,
        request_owner: FactOwnerV1,
    },
    #[error("memory authority returned a result violating {invariant}")]
    InvalidAuthorityResult { invariant: &'static str },
}

#[derive(Debug, Error)]
pub enum MemoryUseCaseError<E: Debug> {
    #[error(transparent)]
    Invariant(#[from] MemoryApplicationInvariantError),
    #[error("memory authority operation failed")]
    Authority(E),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitFactDispositionV1 {
    Committed,
    IdempotentReplay,
    Conflict,
    Unrecognized,
}

#[derive(Clone, Debug)]
pub struct CommitFactCommandV1<C> {
    owner: FactOwnerV1,
    fact_id: FactId,
    command: C,
}

impl<C> CommitFactCommandV1<C> {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, command: C) -> Self {
        Self {
            owner,
            fact_id,
            command,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommitFactPortResultV1<T> {
    output: T,
    disposition: CommitFactDispositionV1,
    receipt_owner: Option<FactOwnerV1>,
    receipt_fact_id: Option<FactId>,
}

impl<T> CommitFactPortResultV1<T> {
    pub fn new(
        output: T,
        disposition: CommitFactDispositionV1,
        receipt_owner: Option<FactOwnerV1>,
        receipt_fact_id: Option<FactId>,
    ) -> Self {
        Self {
            output,
            disposition,
            receipt_owner,
            receipt_fact_id,
        }
    }
}

pub trait CommitFactPort {
    type Command;
    type Error: Debug;
    type Output;

    fn commit_fact(
        &self,
        command: Self::Command,
    ) -> impl Future<Output = Result<CommitFactPortResultV1<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryFactSnapshotV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
    projected_as_of: UtcMicros,
}

impl MemoryFactSnapshotV1 {
    pub const fn new(owner: FactOwnerV1, fact_id: FactId, projected_as_of: UtcMicros) -> Self {
        Self {
            owner,
            fact_id,
            projected_as_of,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryReadCoverageV1 {
    visible: u64,
    hidden: u64,
    unknown: u64,
    redacted: u64,
}

impl MemoryReadCoverageV1 {
    pub const fn new(visible: u64, hidden: u64, unknown: u64, redacted: u64) -> Self {
        Self {
            visible,
            hidden,
            unknown,
            redacted,
        }
    }

    pub const fn visible(self) -> u64 {
        self.visible
    }

    pub const fn hidden(self) -> u64 {
        self.hidden
    }

    pub const fn unknown(self) -> u64 {
        self.unknown
    }

    pub const fn redacted(self) -> u64 {
        self.redacted
    }

    pub const fn is_complete(self) -> bool {
        self.hidden == 0 && self.unknown == 0 && self.redacted == 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryContradictionStateV1 {
    Unknown,
    NotObserved,
    Present { contradicted_by: Vec<FactId> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryReadResultV1<T> {
    payload: T,
    coverage: MemoryReadCoverageV1,
    contradiction: MemoryContradictionStateV1,
}

impl<T> MemoryReadResultV1<T> {
    pub const fn new(
        payload: T,
        coverage: MemoryReadCoverageV1,
        contradiction: MemoryContradictionStateV1,
    ) -> Self {
        Self {
            payload,
            coverage,
            contradiction,
        }
    }

    pub const fn payload(&self) -> &T {
        &self.payload
    }

    pub const fn coverage(&self) -> MemoryReadCoverageV1 {
        self.coverage
    }

    pub const fn contradiction(&self) -> &MemoryContradictionStateV1 {
        &self.contradiction
    }

    pub fn into_payload(self) -> T {
        self.payload
    }
}

#[derive(Clone, Debug)]
pub struct CurrentFactsQueryV1<Q> {
    owner: FactOwnerV1,
    after_fact_id: Option<FactId>,
    limit: usize,
    query: Q,
}

impl<Q> CurrentFactsQueryV1<Q> {
    pub fn new(owner: FactOwnerV1, after_fact_id: Option<FactId>, limit: usize, query: Q) -> Self {
        Self {
            owner,
            after_fact_id,
            limit,
            query,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CurrentFactsPortResultV1<T> {
    output: T,
    snapshots: Vec<MemoryFactSnapshotV1>,
}

impl<T> CurrentFactsPortResultV1<T> {
    pub fn new(output: T, snapshots: Vec<MemoryFactSnapshotV1>) -> Self {
        Self { output, snapshots }
    }
}

pub trait CurrentFactsPort {
    type Error: Debug;
    type Output;
    type Query;

    fn query_current_facts(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<CurrentFactsPortResultV1<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct FactAsOfQueryV1<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    as_of: UtcMicros,
    query: Q,
}

impl<Q> FactAsOfQueryV1<Q> {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, as_of: UtcMicros, query: Q) -> Self {
        Self {
            owner,
            fact_id,
            as_of,
            query,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OptionalFactPortResultV1<T> {
    output: T,
    snapshot: Option<MemoryFactSnapshotV1>,
}

impl<T> OptionalFactPortResultV1<T> {
    pub fn new(output: T, snapshot: Option<MemoryFactSnapshotV1>) -> Self {
        Self { output, snapshot }
    }
}

pub trait FactAsOfPort {
    type Error: Debug;
    type Output;
    type Query;

    fn query_fact_as_of(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<OptionalFactPortResultV1<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct FactCurrentQueryV1<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    query: Q,
}

impl<Q> FactCurrentQueryV1<Q> {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, query: Q) -> Self {
        Self {
            owner,
            fact_id,
            query,
        }
    }
}

pub trait FactCurrentPort {
    type Error: Debug;
    type Output;
    type Query;

    fn query_fact_current(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<OptionalFactPortResultV1<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactLineageCursorV1 {
    occurred_at: UtcMicros,
    event_id: FactEventId,
}

impl FactLineageCursorV1 {
    pub const fn new(occurred_at: UtcMicros, event_id: FactEventId) -> Self {
        Self {
            occurred_at,
            event_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FactLineageQueryV1<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    after: Option<FactLineageCursorV1>,
    limit: usize,
    query: Q,
}

impl<Q> FactLineageQueryV1<Q> {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        after: Option<FactLineageCursorV1>,
        limit: usize,
        query: Q,
    ) -> Self {
        Self {
            owner,
            fact_id,
            after,
            limit,
            query,
        }
    }
}

pub trait FactLineagePort {
    type Error: Debug;
    type Output;
    type Query;

    fn query_fact_lineage(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<FactLineagePortResultV1<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct FactLineagePortResultV1<T> {
    output: T,
    events: Vec<FactLineageEventV1>,
}

impl<T> FactLineagePortResultV1<T> {
    pub fn new(output: T, events: Vec<FactLineageEventV1>) -> Self {
        Self { output, events }
    }
}

#[derive(Clone, Debug)]
pub struct LegacyFactQueryV1<Q> {
    owner: FactOwnerV1,
    query: Q,
}

impl<Q> LegacyFactQueryV1<Q> {
    pub fn new(owner: FactOwnerV1, query: Q) -> Self {
        Self { owner, query }
    }
}

pub trait LegacyFactPort {
    type Error: Debug;
    type Query;

    fn resolve_legacy_fact(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<Option<FactId>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct RetrievalAnchorQueryV1<Q> {
    owner: FactOwnerV1,
    anchor_id: RetrievalAnchorId,
    query: Q,
}

impl<Q> RetrievalAnchorQueryV1<Q> {
    pub fn new(owner: FactOwnerV1, anchor_id: RetrievalAnchorId, query: Q) -> Self {
        Self {
            owner,
            anchor_id,
            query,
        }
    }
}

pub trait RetrievalAnchorPort {
    type Error: Debug;
    type Query;

    fn get_retrieval_anchor(
        &self,
        query: Self::Query,
    ) -> impl Future<Output = Result<Option<RetrievalAnchorRecordV2>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct PromoteFactProposalCommandV1<C, S> {
    owner: FactOwnerV1,
    proposal_id: ProvenanceId,
    expected_state: S,
    fact_id: FactId,
    command: C,
}

impl<C, S> PromoteFactProposalCommandV1<C, S> {
    pub fn new(
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_state: S,
        fact_id: FactId,
        command: C,
    ) -> Self {
        Self {
            owner,
            proposal_id,
            expected_state,
            fact_id,
            command,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PromoteFactProposalPortResultV1<T, S> {
    output: T,
    proposal_id: ProvenanceId,
    previous_state: S,
    commit_disposition: CommitFactDispositionV1,
    receipt_owner: Option<FactOwnerV1>,
    receipt_fact_id: Option<FactId>,
}

impl<T, S> PromoteFactProposalPortResultV1<T, S> {
    pub fn new(
        output: T,
        proposal_id: ProvenanceId,
        previous_state: S,
        commit_disposition: CommitFactDispositionV1,
        receipt_owner: Option<FactOwnerV1>,
        receipt_fact_id: Option<FactId>,
    ) -> Self {
        Self {
            output,
            proposal_id,
            previous_state,
            commit_disposition,
            receipt_owner,
            receipt_fact_id,
        }
    }
}

pub trait PromoteFactProposalPort {
    type Command;
    type Error: Debug;
    type Output;
    type State: Eq;

    fn promote_fact_proposal(
        &self,
        command: Self::Command,
    ) -> impl Future<
        Output = Result<PromoteFactProposalPortResultV1<Self::Output, Self::State>, Self::Error>,
    > + Send;
}

pub struct MemoryApplication<P> {
    owner: FactOwnerV1,
    port: P,
}

impl<P> MemoryApplication<P> {
    pub fn new(owner: FactOwnerV1, port: P) -> Result<Self, MemoryApplicationInvariantError> {
        owner.validate()?;
        Ok(Self { owner, port })
    }

    pub const fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    fn ensure_owner(
        &self,
        request_owner: &FactOwnerV1,
    ) -> Result<(), MemoryApplicationInvariantError> {
        request_owner.validate()?;
        if request_owner != &self.owner {
            return Err(MemoryApplicationInvariantError::OwnerMismatch {
                scope: self.owner.clone(),
                request_owner: request_owner.clone(),
            });
        }
        Ok(())
    }
}

impl<P: CommitFactPort> MemoryApplication<P> {
    pub async fn commit_fact(
        &self,
        command: CommitFactCommandV1<P::Command>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let CommitFactCommandV1 {
            owner,
            fact_id,
            command,
        } = command;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .commit_fact(command)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        validate_commit_result(&owner, &fact_id, &result)?;
        Ok(result.output)
    }
}

impl<P: CurrentFactsPort> MemoryApplication<P> {
    pub async fn query_current_facts(
        &self,
        query: CurrentFactsQueryV1<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let CurrentFactsQueryV1 {
            owner,
            after_fact_id,
            limit,
            query,
        } = query;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .query_current_facts(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if result.snapshots.len() > limit
            || result
                .snapshots
                .iter()
                .any(|snapshot| snapshot.owner != owner)
            || after_fact_id.as_ref().is_some_and(|after_fact_id| {
                result
                    .snapshots
                    .iter()
                    .any(|snapshot| &snapshot.fact_id <= after_fact_id)
            })
            || result
                .snapshots
                .windows(2)
                .any(|pair| pair[0].fact_id >= pair[1].fact_id)
        {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "current fact bounds, owner, cursor, and ordering",
            }
            .into());
        }
        Ok(result.output)
    }
}

impl<P: FactAsOfPort> MemoryApplication<P> {
    pub async fn query_fact_as_of(
        &self,
        query: FactAsOfQueryV1<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let FactAsOfQueryV1 {
            owner,
            fact_id,
            as_of,
            query,
        } = query;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .query_fact_as_of(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if result.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.owner != owner
                || snapshot.fact_id != fact_id
                || snapshot.projected_as_of > as_of
        }) {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "as-of fact identity and timestamp",
            }
            .into());
        }
        Ok(result.output)
    }
}

impl<P: FactCurrentPort> MemoryApplication<P> {
    pub async fn query_fact_current(
        &self,
        query: FactCurrentQueryV1<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let FactCurrentQueryV1 {
            owner,
            fact_id,
            query,
        } = query;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .query_fact_current(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if result
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.owner != owner || snapshot.fact_id != fact_id)
        {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "current fact identity",
            }
            .into());
        }
        Ok(result.output)
    }
}

impl<P: FactLineagePort> MemoryApplication<P> {
    pub async fn query_fact_lineage(
        &self,
        query: FactLineageQueryV1<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let FactLineageQueryV1 {
            owner,
            fact_id,
            after,
            limit,
            query,
        } = query;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .query_fact_lineage(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        let events = &result.events;
        if events.len() > limit
            || events
                .iter()
                .any(|event| event.owner() != &owner || event.fact_id() != &fact_id)
            || after.as_ref().is_some_and(|after| {
                events.iter().any(|event| {
                    (event.occurred_at(), event.event_id()) <= (after.occurred_at, &after.event_id)
                })
            })
            || events.windows(2).any(|pair| {
                (pair[0].occurred_at(), pair[0].event_id())
                    >= (pair[1].occurred_at(), pair[1].event_id())
            })
        {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "fact lineage bounds, owner, cursor, and ordering",
            }
            .into());
        }
        Ok(result.output)
    }
}

impl<P: LegacyFactPort> MemoryApplication<P> {
    pub async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQueryV1<P::Query>,
    ) -> Result<Option<FactId>, MemoryUseCaseError<P::Error>> {
        let LegacyFactQueryV1 { owner, query } = query;
        self.ensure_owner(&owner)?;
        let fact_id = self
            .port
            .resolve_legacy_fact(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if fact_id
            .as_ref()
            .is_some_and(|fact_id| fact_id.validate_owner(&owner).is_err())
        {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "legacy fact owner",
            }
            .into());
        }
        Ok(fact_id)
    }
}

impl<P: RetrievalAnchorPort> MemoryApplication<P> {
    pub async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQueryV1<P::Query>,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryUseCaseError<P::Error>> {
        let RetrievalAnchorQueryV1 {
            owner,
            anchor_id,
            query,
        } = query;
        self.ensure_owner(&owner)?;
        let anchor = self
            .port
            .get_retrieval_anchor(query)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if anchor.as_ref().is_some_and(|anchor| {
            anchor.anchor_id() != &anchor_id || FactOwnerV1::from(anchor.owner().clone()) != owner
        }) {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "retrieval anchor identity",
            }
            .into());
        }
        Ok(anchor)
    }
}

impl<P: PromoteFactProposalPort> MemoryApplication<P> {
    pub async fn promote_fact_proposal(
        &self,
        command: PromoteFactProposalCommandV1<P::Command, P::State>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let PromoteFactProposalCommandV1 {
            owner,
            proposal_id,
            expected_state,
            fact_id,
            command,
        } = command;
        self.ensure_owner(&owner)?;
        let result = self
            .port
            .promote_fact_proposal(command)
            .await
            .map_err(MemoryUseCaseError::Authority)?;
        if result.proposal_id != proposal_id || result.previous_state != expected_state {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "proposal CAS identity",
            }
            .into());
        }
        validate_commit_proof(
            &owner,
            &fact_id,
            result.commit_disposition,
            result.receipt_owner.as_ref(),
            result.receipt_fact_id.as_ref(),
        )?;
        Ok(result.output)
    }
}

fn validate_commit_result<T>(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    result: &CommitFactPortResultV1<T>,
) -> Result<(), MemoryApplicationInvariantError> {
    validate_commit_proof(
        owner,
        fact_id,
        result.disposition,
        result.receipt_owner.as_ref(),
        result.receipt_fact_id.as_ref(),
    )
}

fn validate_commit_proof(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    disposition: CommitFactDispositionV1,
    receipt_owner: Option<&FactOwnerV1>,
    receipt_fact_id: Option<&FactId>,
) -> Result<(), MemoryApplicationInvariantError> {
    let valid = match disposition {
        CommitFactDispositionV1::Committed | CommitFactDispositionV1::IdempotentReplay => {
            receipt_owner == Some(owner) && receipt_fact_id == Some(fact_id)
        }
        CommitFactDispositionV1::Conflict => receipt_owner.is_none() && receipt_fact_id.is_none(),
        CommitFactDispositionV1::Unrecognized => {
            return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
                invariant: "recognized fact commit outcome",
            });
        }
    };
    if !valid {
        return Err(MemoryApplicationInvariantError::InvalidAuthorityResult {
            invariant: "fact commit identity",
        });
    }
    Ok(())
}
