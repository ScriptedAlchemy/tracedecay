//! Canonical persistence port for Git topology retrieval anchors.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::{
    ObservationScopeV1, RetrievalAnchorId, RetrievalAnchorRecord, RetrievalAnchorTarget,
};

pub const MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTopologyAnchorPublication {
    owner: ObservationScopeV1,
    records: Vec<RetrievalAnchorRecord>,
}

impl GitTopologyAnchorPublication {
    pub fn new(
        owner: ObservationScopeV1,
        records: Vec<RetrievalAnchorRecord>,
    ) -> Result<Self, GitTopologyAnchorAuthorityError> {
        owner
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
        if records.is_empty() || records.len() > MAX_GIT_TOPOLOGY_ANCHORS_PER_PUBLICATION {
            return Err(GitTopologyAnchorAuthorityError::Conflict);
        }
        let mut has_topology = false;
        let mut anchor_ids = BTreeSet::new();
        for record in &records {
            record
                .validate()
                .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
            if record.owner() != &owner || !record.aliases().is_empty() {
                return Err(GitTopologyAnchorAuthorityError::Conflict);
            }
            if !anchor_ids.insert(record.anchor_id().clone()) {
                return Err(GitTopologyAnchorAuthorityError::Conflict);
            }
            match record.target() {
                RetrievalAnchorTarget::GitTopology(_) => has_topology = true,
                RetrievalAnchorTarget::ExactRepositoryCommit { .. } => {}
                _ => return Err(GitTopologyAnchorAuthorityError::Conflict),
            }
        }
        if !has_topology {
            return Err(GitTopologyAnchorAuthorityError::Conflict);
        }
        if records.iter().any(|record| {
            record
                .source_anchors()
                .iter()
                .any(|source| !anchor_ids.contains(source.anchor_id()))
        }) {
            return Err(GitTopologyAnchorAuthorityError::Conflict);
        }
        Ok(Self { owner, records })
    }

    pub fn owner(&self) -> &ObservationScopeV1 {
        &self.owner
    }

    pub fn records(&self) -> &[RetrievalAnchorRecord] {
        &self.records
    }

    pub fn into_records(self) -> Vec<RetrievalAnchorRecord> {
        self.records
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTopologyAnchorResolution {
    pub owner: ObservationScopeV1,
    pub anchor_id: RetrievalAnchorId,
}

impl GitTopologyAnchorResolution {
    pub fn new(
        owner: ObservationScopeV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<Self, GitTopologyAnchorAuthorityError> {
        owner
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
        anchor_id
            .validate()
            .map_err(|_| GitTopologyAnchorAuthorityError::Conflict)?;
        Ok(Self { owner, anchor_id })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorPublicationOutcome {
    Published,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorResolutionOutcome {
    Resolved(Box<RetrievalAnchorRecord>),
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTopologyAnchorAuthorityError {
    Unavailable,
    ResetRequired,
    Conflict,
}

pub type GitTopologyAnchorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, GitTopologyAnchorAuthorityError>> + Send + 'a>>;

pub trait GitTopologyAnchorAuthority: Send + Sync {
    fn publish<'a>(
        &'a self,
        publication: GitTopologyAnchorPublication,
    ) -> GitTopologyAnchorFuture<'a, GitTopologyAnchorPublicationOutcome>;

    fn resolve<'a>(
        &'a self,
        resolution: GitTopologyAnchorResolution,
    ) -> GitTopologyAnchorFuture<'a, GitTopologyAnchorResolutionOutcome>;
}
