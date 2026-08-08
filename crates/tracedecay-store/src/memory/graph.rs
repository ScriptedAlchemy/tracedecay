use std::collections::BTreeSet;
use std::future::Future;

use tracedecay_domain::{DomainError, FactAssertionId, FactId, FactOwnerV1, RetrievalAnchorId};

use super::{
    FactStoreError, FactStoreResult, ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryLegacyEntityTargetV1, ProjectMemoryResult,
};

pub const MAX_PROJECT_MEMORY_GRAPH_RELATIONS: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryGraphQueryV1 {
    owner: FactOwnerV1,
    roots: Vec<FactId>,
    max_relations: usize,
}

impl ProjectMemoryGraphQueryV1 {
    pub fn new(
        owner: FactOwnerV1,
        roots: Vec<FactId>,
        max_relations: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if max_relations == 0 || max_relations > MAX_PROJECT_MEMORY_GRAPH_RELATIONS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: max_relations,
                max: MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
            });
        }
        if roots.len() > MAX_PROJECT_MEMORY_GRAPH_RELATIONS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: roots.len(),
                max: MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
            });
        }
        for root in &roots {
            root.validate()?;
            root.validate_owner(&owner)
                .map_err(|_| FactStoreError::OwnerMismatch)?;
        }
        if roots.iter().collect::<BTreeSet<_>>().len() != roots.len() {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory graph roots",
            }));
        }
        Ok(Self {
            owner,
            roots,
            max_relations,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn roots(&self) -> &[FactId] {
        &self.roots
    }

    pub fn max_relations(&self) -> usize {
        self.max_relations
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectMemoryGraphRelationKindV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
    Mentions,
    ActiveAssertion,
    EvidenceAnchor,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectMemoryGraphTargetV1 {
    Fact(ProjectMemoryFactIdV1),
    Entity(ProjectMemoryLegacyEntityTargetV1),
    Assertion {
        owner: FactOwnerV1,
        fact_id: FactId,
        assertion_id: FactAssertionId,
    },
    RetrievalAnchor {
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    },
}

impl ProjectMemoryGraphTargetV1 {
    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Fact(target) => target.owner(),
            Self::Entity(target) => target.owner(),
            Self::Assertion { owner, .. } | Self::RetrievalAnchor { owner, .. } => owner,
        }
    }

    fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if self.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        match self {
            Self::Fact(target) => target.fact_id().validate_owner(owner)?,
            Self::Entity(_) => {}
            Self::Assertion {
                fact_id,
                assertion_id,
                ..
            } => {
                fact_id.validate_owner(owner)?;
                assertion_id.validate()?;
            }
            Self::RetrievalAnchor { anchor_id, .. } => anchor_id.validate()?,
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryGraphRelationV1 {
    source: ProjectMemoryGraphTargetV1,
    target: ProjectMemoryGraphTargetV1,
    kind: ProjectMemoryGraphRelationKindV1,
}

impl ProjectMemoryGraphRelationV1 {
    pub fn new(
        owner: &FactOwnerV1,
        source: ProjectMemoryGraphTargetV1,
        target: ProjectMemoryGraphTargetV1,
        kind: ProjectMemoryGraphRelationKindV1,
    ) -> FactStoreResult<Self> {
        source.validate_for_owner(owner)?;
        target.validate_for_owner(owner)?;
        if source == target {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory graph relation endpoints",
            }));
        }
        Ok(Self {
            source,
            target,
            kind,
        })
    }

    pub fn source(&self) -> &ProjectMemoryGraphTargetV1 {
        &self.source
    }

    pub fn target(&self) -> &ProjectMemoryGraphTargetV1 {
        &self.target
    }

    pub fn kind(&self) -> ProjectMemoryGraphRelationKindV1 {
        self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryGraphPageV1 {
    owner: FactOwnerV1,
    facts: Vec<ProjectMemoryFactProjectionV1>,
    relations: Vec<ProjectMemoryGraphRelationV1>,
}

impl ProjectMemoryGraphPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        facts: Vec<ProjectMemoryFactProjectionV1>,
        relations: Vec<ProjectMemoryGraphRelationV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if facts.iter().any(|fact| fact.owner() != &owner) {
            return Err(FactStoreError::OwnerMismatch);
        }
        for relation in &relations {
            relation.source().validate_for_owner(&owner)?;
            relation.target().validate_for_owner(&owner)?;
        }
        if relations.len() > MAX_PROJECT_MEMORY_GRAPH_RELATIONS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: relations.len(),
                max: MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
            });
        }
        Ok(Self {
            owner,
            facts,
            relations,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn facts(&self) -> &[ProjectMemoryFactProjectionV1] {
        &self.facts
    }

    pub fn relations(&self) -> &[ProjectMemoryGraphRelationV1] {
        &self.relations
    }
}

pub trait ProjectMemoryGraphStore: Send + Sync {
    fn project_memory_graph(
        &self,
        query: ProjectMemoryGraphQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryGraphPageV1>> + Send;
}
