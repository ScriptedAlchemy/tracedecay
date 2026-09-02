use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactId, FactOwnerV1, ManifestDigest, ProvenanceId,
    UtcMicros, canonical_sha256,
};

use super::super::queries::{MAX_CURRENT_LIMIT, validate_limit};
use super::super::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_REASON_BYTES, validate_owned_fact_id,
};
use super::{
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactV1,
    validate_project_memory_entity,
};

pub const MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS: u32 = 1_500_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactSearchKindV1 {
    Search,
    Probe,
    /// Co-occurrence expansion: resolve entities sharing a fact with the
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
                        field: "project memory fact reason entities",
                    }));
                }
                let mut previous: Option<&String> = None;
                for entity in entities {
                    validate_project_memory_entity(entity)?;
                    if previous.is_some_and(|value| value >= entity) {
                        return Err(FactStoreError::Contract(DomainError::NonCanonical {
                            field: "project memory fact reason entities",
                        }));
                    }
                    previous = Some(entity);
                }
            }
        }
        Ok(())
    }
}

/// Optional deterministic constraints applied before project-memory ranking.
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
        if threshold_millionths
            .is_some_and(|value| value > MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact search threshold",
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

/// Exclusive continuation token for score-descending project-memory retrieval.
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
        if score_millionths > MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact search cursor score",
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

/// One scored project-memory search result. Scores are fixed-point millionths,
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
        if score_millionths > MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS
            || [
                fts_score_millionths,
                jaccard_score_millionths,
                holographic_score_millionths,
                trust_score_millionths,
            ]
            .into_iter()
            .any(|value| value > 1_000_000)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact search score",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactSearchGraphDegradationV1 {
    Conflict,
    Unavailable,
    BudgetExhausted,
    DeadlineExceeded,
}

/// Exact graph-assist coverage for one fact-search page. `NotMounted` is
/// distinct from a mounted authority that degraded while publishing or
/// reading its verified generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactSearchGraphCoverageV1 {
    NotApplicable,
    NotMounted,
    Complete {
        root_count: usize,
        relation_count: usize,
        expanded_fact_count: usize,
    },
    Degraded {
        reason: ProjectMemoryFactSearchGraphDegradationV1,
    },
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
                field: "project memory fact search why",
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
    graph_coverage: ProjectMemoryFactSearchGraphCoverageV1,
}

impl ProjectMemoryFactSearchPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        hits: Vec<ProjectMemoryFactSearchHitV1>,
        next_after: Option<ProjectMemoryFactSearchCursorV1>,
        graph_coverage: ProjectMemoryFactSearchGraphCoverageV1,
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
                    field: "project memory fact search order",
                }));
            }
            previous = Some(hit);
        }
        if let Some(cursor) = &next_after {
            validate_owned_fact_id(cursor.fact_id(), &owner)?;
            let Some(last) = hits.last() else {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "project memory fact search cursor without hits",
                }));
            };
            if cursor.score_millionths() != last.score_millionths()
                || cursor.updated_at() != last.fact().telemetry().updated_at()
                || cursor.fact_id() != last.fact().fact_id()
            {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "project memory fact search cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            hits,
            next_after,
            graph_coverage,
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
    pub fn graph_coverage(&self) -> ProjectMemoryFactSearchGraphCoverageV1 {
        self.graph_coverage
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
                field: "project memory fact contradiction threshold",
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
                field: "project memory fact contradiction",
            }));
        }
        if why.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact contradiction reason",
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
    targets: Vec<ProjectMemoryFactIdV1>,
    recall: bool,
}

impl ProjectMemoryFactRetrievalCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        targets: Vec<ProjectMemoryFactIdV1>,
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
        if targets.iter().enumerate().any(|(index, target)| {
            targets[..index]
                .iter()
                .any(|previous| previous.fact_id() == target.fact_id())
        }) {
            return Err(FactStoreError::Contract(DomainError::DuplicateId {
                field: "project memory fact retrieval targets",
            }));
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
    pub fn targets(&self) -> &[ProjectMemoryFactIdV1] {
        &self.targets
    }
    pub fn recall(&self) -> bool {
        self.recall
    }

    pub fn input_digest(&self) -> FactStoreResult<String> {
        let targets = self
            .targets
            .iter()
            .map(|target| (target.owner(), target.fact_id()))
            .collect::<Vec<_>>();
        let digest = canonical_sha256(&(
            "tracedecay.project-memory.fact-retrieval-input.v1",
            &self.owner,
            targets,
            self.recall,
        ))?;
        digest
            .as_str()
            .strip_prefix("sha256:")
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                FactStoreError::Contract(DomainError::NonCanonical {
                    field: "project memory fact retrieval input digest",
                })
            })
    }
}

/// Durable identity for one retrieval telemetry mutation. Receipt hashers use
/// the stable accessors and exclude `replayed`, which is delivery metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRetrievalReceiptV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    fact_ids: Vec<ProjectMemoryFactIdV1>,
    recall: bool,
    replayed: bool,
    committed_state_digest: ManifestDigest,
}

impl ProjectMemoryFactRetrievalReceiptV1 {
    pub fn recorded(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        fact_ids: Vec<ProjectMemoryFactIdV1>,
        recall: bool,
    ) -> FactStoreResult<Self> {
        Self::build(owner, operation_id, input_digest, fact_ids, recall, false)
    }

    pub fn from_replay(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        fact_ids: Vec<ProjectMemoryFactIdV1>,
        recall: bool,
    ) -> FactStoreResult<Self> {
        Self::build(owner, operation_id, input_digest, fact_ids, recall, true)
    }

    fn build(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        fact_ids: Vec<ProjectMemoryFactIdV1>,
        recall: bool,
        replayed: bool,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if input_digest.len() != 64
            || !input_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact retrieval input digest",
            }));
        }
        if fact_ids.is_empty() || fact_ids.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: fact_ids.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        if fact_ids.iter().any(|fact_id| fact_id.owner() != &owner) {
            return Err(FactStoreError::OwnerMismatch);
        }
        if fact_ids.iter().enumerate().any(|(index, fact_id)| {
            fact_ids[..index]
                .iter()
                .any(|previous| previous.fact_id() == fact_id.fact_id())
        }) {
            return Err(FactStoreError::Contract(DomainError::DuplicateId {
                field: "project memory fact retrieval receipt fact ids",
            }));
        }
        let durable_fact_ids = fact_ids
            .iter()
            .map(ProjectMemoryFactIdV1::fact_id)
            .collect::<Vec<_>>();
        let committed_state_digest = canonical_sha256(&(
            "tracedecay.project-memory.fact-retrieval-receipt.committed-state.v1",
            &owner,
            &operation_id,
            &input_digest,
            durable_fact_ids,
            recall,
        ))?;
        Ok(Self {
            owner,
            operation_id,
            input_digest,
            fact_ids,
            recall,
            replayed,
            committed_state_digest,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    pub fn fact_ids(&self) -> &[ProjectMemoryFactIdV1] {
        &self.fact_ids
    }

    pub fn recall(&self) -> bool {
        self.recall
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }

    /// Infallible digest of the validated durable receipt fields. The
    /// delivery-only replay disposition and hydrated projections are excluded.
    pub fn committed_state_digest(&self) -> &ManifestDigest {
        &self.committed_state_digest
    }
}

/// Receipt-bearing retrieval result. Projections are hydrated from current
/// canonical state and never persisted inside the operation receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRetrievalOutcomeV1 {
    receipt: ProjectMemoryFactRetrievalReceiptV1,
    projections: Vec<ProjectMemoryFactProjectionV1>,
}

impl ProjectMemoryFactRetrievalOutcomeV1 {
    pub fn new(
        receipt: ProjectMemoryFactRetrievalReceiptV1,
        projections: Vec<ProjectMemoryFactProjectionV1>,
    ) -> FactStoreResult<Self> {
        if receipt.fact_ids().len() != projections.len()
            || receipt
                .fact_ids()
                .iter()
                .zip(&projections)
                .any(|(fact_id, projection)| {
                    fact_id.owner() != projection.owner()
                        || fact_id.fact_id() != projection.fact_id()
                })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact retrieval outcome projections",
            }));
        }
        Ok(Self {
            receipt,
            projections,
        })
    }

    pub fn receipt(&self) -> &ProjectMemoryFactRetrievalReceiptV1 {
        &self.receipt
    }

    pub fn projections(&self) -> &[ProjectMemoryFactProjectionV1] {
        &self.projections
    }
}
