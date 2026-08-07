use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
};

use super::super::queries::{MAX_CURRENT_LIMIT, validate_limit};
use super::super::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_REASON_BYTES, validate_owned_fact_id,
};
use super::{ProjectMemoryFactTargetV1, ProjectMemoryFactV1, validate_project_memory_entity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactSearchKindV1 {
    Search,
    Probe,
    /// V1 co-occurrence expansion: resolve entities sharing a fact with the
    /// source entity, then probe those entities. This is not a direct source
    /// entity filter.
    Related {
        entity: String,
    },
    Reason {
        entities: Vec<String>,
    },
}

impl ProjectMemoryFactSearchKindV1 {
    pub(in crate::memory) fn validate(&self) -> FactStoreResult<()> {
        match self {
            Self::Search | Self::Probe => {}
            Self::Related { entity } => validate_project_memory_entity(entity)?,
            Self::Reason { entities } => {
                if entities.is_empty() || entities.len() > MAX_CURRENT_LIMIT {
                    return Err(FactStoreError::Contract(DomainError::NonCanonical {
                        field: "compatibility fact reason entities",
                    }));
                }
                let mut previous: Option<&String> = None;
                for entity in entities {
                    validate_project_memory_entity(entity)?;
                    if previous.is_some_and(|value| value >= entity) {
                        return Err(FactStoreError::Contract(DomainError::NonCanonical {
                            field: "compatibility fact reason entities",
                        }));
                    }
                    previous = Some(entity);
                }
            }
        }
        Ok(())
    }
}

/// Optional deterministic constraints applied before compatibility ranking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchFilterV1 {
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    threshold_millionths: Option<u32>,
}

impl ProjectMemoryFactSearchFilterV1 {
    pub fn new(
        category: Option<FactCategoryV1>,
        min_trust: Option<Confidence>,
        threshold_millionths: Option<u32>,
    ) -> FactStoreResult<Self> {
        if threshold_millionths.is_some_and(|value| value > 1_000_000) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search threshold",
            }));
        }
        Ok(Self {
            category,
            min_trust,
            threshold_millionths,
        })
    }

    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }

    pub fn min_trust(&self) -> Option<Confidence> {
        self.min_trust
    }

    pub fn threshold_millionths(&self) -> Option<u32> {
        self.threshold_millionths
    }
}

/// Exclusive continuation token for score-descending compatibility retrieval.
/// The fact ID breaks equal-score ties, so a page can resume deterministically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchCursorV1 {
    score_millionths: u32,
    updated_at: UtcMicros,
    fact_id: FactId,
}

impl ProjectMemoryFactSearchCursorV1 {
    pub fn new(
        score_millionths: u32,
        updated_at: UtcMicros,
        fact_id: FactId,
    ) -> FactStoreResult<Self> {
        if score_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search cursor score",
            }));
        }
        fact_id.validate()?;
        Ok(Self {
            score_millionths,
            updated_at,
            fact_id,
        })
    }

    pub fn score_millionths(&self) -> u32 {
        self.score_millionths
    }

    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
}

/// One scored compatibility search result.  Scores are fixed-point millionths,
/// avoiding non-deterministic floating point ordering at the transport edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchScoresV1 {
    score_millionths: u32,
    fts_score_millionths: u32,
    jaccard_score_millionths: u32,
    holographic_score_millionths: u32,
    trust_score_millionths: u32,
}

impl ProjectMemoryFactSearchScoresV1 {
    pub fn new(
        score_millionths: u32,
        fts_score_millionths: u32,
        jaccard_score_millionths: u32,
        holographic_score_millionths: u32,
        trust_score_millionths: u32,
    ) -> FactStoreResult<Self> {
        if [
            score_millionths,
            fts_score_millionths,
            jaccard_score_millionths,
            holographic_score_millionths,
            trust_score_millionths,
        ]
        .into_iter()
        .any(|value| value > 1_000_000)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search score",
            }));
        }
        Ok(Self {
            score_millionths,
            fts_score_millionths,
            jaccard_score_millionths,
            holographic_score_millionths,
            trust_score_millionths,
        })
    }

    pub fn score_millionths(self) -> u32 {
        self.score_millionths
    }
    pub fn fts_score_millionths(self) -> u32 {
        self.fts_score_millionths
    }
    pub fn jaccard_score_millionths(self) -> u32 {
        self.jaccard_score_millionths
    }
    pub fn holographic_score_millionths(self) -> u32 {
        self.holographic_score_millionths
    }
    pub fn trust_score_millionths(self) -> u32 {
        self.trust_score_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchHitV1 {
    fact: ProjectMemoryFactV1,
    scores: ProjectMemoryFactSearchScoresV1,
    why: Option<String>,
}

impl ProjectMemoryFactSearchHitV1 {
    pub fn new(
        fact: ProjectMemoryFactV1,
        scores: ProjectMemoryFactSearchScoresV1,
        why: Option<String>,
    ) -> FactStoreResult<Self> {
        if why.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search why",
            }));
        }
        Ok(Self { fact, scores, why })
    }

    pub fn fact(&self) -> &ProjectMemoryFactV1 {
        &self.fact
    }
    pub fn score_millionths(&self) -> u32 {
        self.scores.score_millionths()
    }
    pub fn scores(&self) -> ProjectMemoryFactSearchScoresV1 {
        self.scores
    }
    pub fn why(&self) -> Option<&str> {
        self.why.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchPageV1 {
    owner: FactOwnerV1,
    hits: Vec<ProjectMemoryFactSearchHitV1>,
    next_after: Option<ProjectMemoryFactSearchCursorV1>,
}

impl ProjectMemoryFactSearchPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        hits: Vec<ProjectMemoryFactSearchHitV1>,
        next_after: Option<ProjectMemoryFactSearchCursorV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if hits.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: hits.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&ProjectMemoryFactSearchHitV1> = None;
        for hit in &hits {
            hit.fact().validate_for_owner(&owner)?;
            if previous.is_some_and(|value| {
                value.score_millionths() < hit.score_millionths()
                    || (value.score_millionths() == hit.score_millionths()
                        && (value.fact().telemetry().updated_at()
                            < hit.fact().telemetry().updated_at()
                            || (value.fact().telemetry().updated_at()
                                == hit.fact().telemetry().updated_at()
                                && value.fact().fact_id() >= hit.fact().fact_id())))
            }) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search order",
                }));
            }
            previous = Some(hit);
        }
        if let Some(cursor) = &next_after {
            validate_owned_fact_id(cursor.fact_id(), &owner)?;
            let Some(last) = hits.last() else {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search cursor without hits",
                }));
            };
            if cursor.score_millionths() != last.score_millionths()
                || cursor.updated_at() != last.fact().telemetry().updated_at()
                || cursor.fact_id() != last.fact().fact_id()
            {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            hits,
            next_after,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn hits(&self) -> &[ProjectMemoryFactSearchHitV1] {
        &self.hits
    }
    pub fn next_after(&self) -> Option<&ProjectMemoryFactSearchCursorV1> {
        self.next_after.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactContradictionQueryV1 {
    owner: FactOwnerV1,
    category: Option<FactCategoryV1>,
    threshold_millionths: u32,
    limit: usize,
}

impl ProjectMemoryFactContradictionQueryV1 {
    pub fn new(
        owner: FactOwnerV1,
        category: Option<FactCategoryV1>,
        threshold_millionths: u32,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if threshold_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction threshold",
            }));
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            category,
            threshold_millionths,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn threshold_millionths(&self) -> u32 {
        self.threshold_millionths
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactContradictionV1 {
    existing: ProjectMemoryFactV1,
    new_content: String,
    score_millionths: u32,
    why: Option<String>,
}

impl ProjectMemoryFactContradictionV1 {
    pub fn new(
        existing: ProjectMemoryFactV1,
        new_content: String,
        score_millionths: u32,
        why: Option<String>,
    ) -> FactStoreResult<Self> {
        if new_content.trim().is_empty() || score_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction",
            }));
        }
        if why.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction reason",
            }));
        }
        Ok(Self {
            existing,
            new_content,
            score_millionths,
            why,
        })
    }

    pub fn existing(&self) -> &ProjectMemoryFactV1 {
        &self.existing
    }
    pub fn new_content(&self) -> &str {
        &self.new_content
    }
    pub fn score_millionths(&self) -> u32 {
        self.score_millionths
    }
    pub fn why(&self) -> Option<&str> {
        self.why.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactContradictionPageV1 {
    owner: FactOwnerV1,
    contradictions: Vec<ProjectMemoryFactContradictionV1>,
}

impl ProjectMemoryFactContradictionPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        contradictions: Vec<ProjectMemoryFactContradictionV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if contradictions.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: contradictions.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        for contradiction in &contradictions {
            if contradiction.existing().owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        Ok(Self {
            owner,
            contradictions,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn contradictions(&self) -> &[ProjectMemoryFactContradictionV1] {
        &self.contradictions
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRetrievalCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    targets: Vec<ProjectMemoryFactTargetV1>,
    recall: bool,
}

impl ProjectMemoryFactRetrievalCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        targets: Vec<ProjectMemoryFactTargetV1>,
        recall: bool,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if targets.is_empty() || targets.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: targets.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        if targets.iter().any(|target| target.owner() != &owner) {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(Self {
            owner,
            operation_id,
            targets,
            recall,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn targets(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.targets
    }
    pub fn recall(&self) -> bool {
        self.recall
    }
}
