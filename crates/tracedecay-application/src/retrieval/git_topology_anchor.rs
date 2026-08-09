//! Canonical V2 persistence port for Git topology retrieval anchors.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::{
    ObservationScopeV1, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorTargetV2,
};

pub const MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION_V2: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTopologyAnchorPublicationV2 {
    owner: ObservationScopeV1,
    records: Vec<RetrievalAnchorRecordV2>,
}

impl GitTopologyAnchorPublicationV2 {
    pub fn new(
        owner: ObservationScopeV1,
        records: Vec<RetrievalAnchorRecordV2>,
    ) -> Result<Self, GitTopologyAnchorAuthorityErrorV2> {
        owner
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
        if records.is_empty() || records.len() > MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION_V2 {
            return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
        }
        let mut has_topology = false;
        let mut anchor_ids = BTreeSet::new();
        for record in &records {
            record
                .validate()
                .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
            if record.owner() != &owner || !record.aliases().is_empty() {
                return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
            }
            if !anchor_ids.insert(record.anchor_id().clone()) {
                return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
            }
            match record.target() {
                RetrievalAnchorTargetV2::GitTopology(_) => has_topology = true,
                RetrievalAnchorTargetV2::ExactRepositoryCommit { .. } => {}
                _ => return Err(GitTopologyAnchorAuthorityErrorV2::Conflict),
            }
        }
        if !has_topology {
            return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
        }
        if records.iter().any(|record| {
            record
                .source_anchors()
                .iter()
                .any(|source| !anchor_ids.contains(source.anchor_id()))
        }) {
            return Err(GitTopologyAnchorAuthorityErrorV2::Conflict);
        }
        Ok(Self { owner, records })
    }

    pub fn owner(&self) -> &ObservationScopeV1 {
        &self.owner
    }

    pub fn records(&self) -> &[RetrievalAnchorRecordV2] {
        &self.records
    }

    pub fn into_records(self) -> Vec<RetrievalAnchorRecordV2> {
        self.records
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTopologyAnchorResolutionV2 {
    pub owner: ObservationScopeV1,
    pub anchor_id: RetrievalAnchorId,
}

impl GitTopologyAnchorResolutionV2 {
    pub fn new(
        owner: ObservationScopeV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<Self, GitTopologyAnchorAuthorityErrorV2> {
        owner
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
        anchor_id
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityErrorV2::Conflict)?;
        Ok(Self { owner, anchor_id })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorPublicationOutcomeV2 {
    Published,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorResolutionOutcomeV2 {
    Resolved(RetrievalAnchorRecordV2),
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorAuthorityErrorV2 {
    Unavailable,
    ResetRequired,
    Conflict,
}

pub type GitTopologyAnchorFutureV2<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, GitTopologyAnchorAuthorityErrorV2>> + Send + 'a>>;

pub trait GitTopologyAnchorAuthorityV2: Send + Sync {
    fn publish<'a>(
        &'a self,
        publication: GitTopologyAnchorPublicationV2,
    ) -> GitTopologyAnchorFutureV2<'a, GitTopologyAnchorPublicationOutcomeV2>;

    fn resolve<'a>(
        &'a self,
        resolution: GitTopologyAnchorResolutionV2,
    ) -> GitTopologyAnchorFutureV2<'a, GitTopologyAnchorResolutionOutcomeV2>;
}
