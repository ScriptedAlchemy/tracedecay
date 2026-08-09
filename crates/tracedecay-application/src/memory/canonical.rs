//! Owner-bound canonical memory use cases over transport-neutral ports.

use std::fmt::Debug;
use std::future::Future;

use thiserror::Error;
use tracedecay_domain::{
    DomainError, FactEventId, FactId, FactLineageEventV1, FactOwnerV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2, UtcMicros,
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
pub enum MemoryCommitFactDisposition {
    Committed,
    IdempotentReplay,
    Conflict,
    Unrecognized,
}

#[derive(Clone, Debug)]
pub struct MemoryCommitFactCommand<C> {
    owner: FactOwnerV1,
    fact_id: FactId,
    command: C,
}

impl<C> MemoryCommitFactCommand<C> {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, command: C) -> Self {
        Self {
            owner,
            fact_id,
            command,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryCommitFactPortResult<T> {
    output: T,
    disposition: MemoryCommitFactDisposition,
    receipt_owner: Option<FactOwnerV1>,
    receipt_fact_id: Option<FactId>,
}

impl<T> MemoryCommitFactPortResult<T> {
    pub fn new(
        output: T,
        disposition: MemoryCommitFactDisposition,
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
    ) -> impl Future<Output = Result<MemoryCommitFactPortResult<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryFactSnapshot {
    owner: FactOwnerV1,
    fact_id: FactId,
    projected_as_of: UtcMicros,
}

impl MemoryFactSnapshot {
    pub const fn new(owner: FactOwnerV1, fact_id: FactId, projected_as_of: UtcMicros) -> Self {
        Self {
            owner,
            fact_id,
            projected_as_of,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemoryReadCoverage {
    visible: u64,
    hidden: u64,
    unknown: u64,
    redacted: u64,
}

impl MemoryReadCoverage {
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
pub enum MemoryContradictionState {
    Unknown,
    NotObserved,
    Present { contradicted_by: Vec<FactId> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryReadResult<T> {
    payload: T,
    coverage: MemoryReadCoverage,
    contradiction: MemoryContradictionState,
}

impl<T> MemoryReadResult<T> {
    pub const fn new(
        payload: T,
        coverage: MemoryReadCoverage,
        contradiction: MemoryContradictionState,
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

    pub const fn coverage(&self) -> MemoryReadCoverage {
        self.coverage
    }

    pub const fn contradiction(&self) -> &MemoryContradictionState {
        &self.contradiction
    }

    pub fn into_payload(self) -> T {
        self.payload
    }
}

#[derive(Clone, Debug)]
pub struct MemoryCurrentFactsQuery<Q> {
    owner: FactOwnerV1,
    after_fact_id: Option<FactId>,
    limit: usize,
    query: Q,
}

impl<Q> MemoryCurrentFactsQuery<Q> {
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
pub struct MemoryCurrentFactsPortResult<T> {
    output: T,
    snapshots: Vec<MemoryFactSnapshot>,
}

impl<T> MemoryCurrentFactsPortResult<T> {
    pub fn new(output: T, snapshots: Vec<MemoryFactSnapshot>) -> Self {
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
    ) -> impl Future<Output = Result<MemoryCurrentFactsPortResult<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct MemoryFactAsOfQuery<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    as_of: UtcMicros,
    query: Q,
}

impl<Q> MemoryFactAsOfQuery<Q> {
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
pub struct MemoryOptionalFactPortResult<T> {
    output: T,
    snapshot: Option<MemoryFactSnapshot>,
}

impl<T> MemoryOptionalFactPortResult<T> {
    pub fn new(output: T, snapshot: Option<MemoryFactSnapshot>) -> Self {
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
    ) -> impl Future<Output = Result<MemoryOptionalFactPortResult<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct MemoryFactCurrentQuery<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    query: Q,
}

impl<Q> MemoryFactCurrentQuery<Q> {
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
    ) -> impl Future<Output = Result<MemoryOptionalFactPortResult<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryFactLineageCursor {
    occurred_at: UtcMicros,
    event_id: FactEventId,
}

impl MemoryFactLineageCursor {
    pub const fn new(occurred_at: UtcMicros, event_id: FactEventId) -> Self {
        Self {
            occurred_at,
            event_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryFactLineageQuery<Q> {
    owner: FactOwnerV1,
    fact_id: FactId,
    after: Option<MemoryFactLineageCursor>,
    limit: usize,
    query: Q,
}

impl<Q> MemoryFactLineageQuery<Q> {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        after: Option<MemoryFactLineageCursor>,
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
    ) -> impl Future<Output = Result<MemoryFactLineagePortResult<Self::Output>, Self::Error>> + Send;
}

#[derive(Clone, Debug)]
pub struct MemoryFactLineagePortResult<T> {
    output: T,
    events: Vec<FactLineageEventV1>,
}

impl<T> MemoryFactLineagePortResult<T> {
    pub fn new(output: T, events: Vec<FactLineageEventV1>) -> Self {
        Self { output, events }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryLegacyFactQuery<Q> {
    owner: FactOwnerV1,
    query: Q,
}

impl<Q> MemoryLegacyFactQuery<Q> {
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
pub struct MemoryRetrievalAnchorQuery<Q> {
    owner: FactOwnerV1,
    anchor_id: RetrievalAnchorId,
    query: Q,
}

impl<Q> MemoryRetrievalAnchorQuery<Q> {
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
        command: MemoryCommitFactCommand<P::Command>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let MemoryCommitFactCommand {
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
        query: MemoryCurrentFactsQuery<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let MemoryCurrentFactsQuery {
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
        query: MemoryFactAsOfQuery<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let MemoryFactAsOfQuery {
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
        query: MemoryFactCurrentQuery<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let MemoryFactCurrentQuery {
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
        query: MemoryFactLineageQuery<P::Query>,
    ) -> Result<P::Output, MemoryUseCaseError<P::Error>> {
        let MemoryFactLineageQuery {
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
        query: MemoryLegacyFactQuery<P::Query>,
    ) -> Result<Option<FactId>, MemoryUseCaseError<P::Error>> {
        let MemoryLegacyFactQuery { owner, query } = query;
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
        query: MemoryRetrievalAnchorQuery<P::Query>,
    ) -> Result<Option<RetrievalAnchorRecordV2>, MemoryUseCaseError<P::Error>> {
        let MemoryRetrievalAnchorQuery {
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

fn validate_commit_result<T>(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    result: &MemoryCommitFactPortResult<T>,
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
    disposition: MemoryCommitFactDisposition,
    receipt_owner: Option<&FactOwnerV1>,
    receipt_fact_id: Option<&FactId>,
) -> Result<(), MemoryApplicationInvariantError> {
    let valid = match disposition {
        MemoryCommitFactDisposition::Committed | MemoryCommitFactDisposition::IdempotentReplay => {
            receipt_owner == Some(owner) && receipt_fact_id == Some(fact_id)
        }
        MemoryCommitFactDisposition::Conflict => {
            receipt_owner.is_none() && receipt_fact_id.is_none()
        }
        MemoryCommitFactDisposition::Unrecognized => {
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
