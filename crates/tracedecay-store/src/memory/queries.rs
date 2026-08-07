use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactEventId, FactId, FactLineageEventV1, FactOwnerV1,
    LocatorDigest, RetrievalAnchorId, SourceStoreId, UtcMicros,
};

use super::{
    FactStoreError, FactStoreResult, MAX_COMPATIBILITY_SEARCH_BYTES,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchFilterV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactTargetV1, StoredFactV1, validate_owned_fact_id,
};

pub(super) const MAX_CURRENT_LIMIT: usize = 1_000;

/// Maximum sorted, deduplicated contradiction identifiers in one response snapshot.
pub const MAX_FACT_QUERY_CONTRADICTIONS: usize = 1_000;

pub(super) const MAX_LINEAGE_LIMIT: usize = MAX_FACT_QUERY_CONTRADICTIONS;

/// Exact frontier denominators plus redaction counts for one fact query snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FactQueryCoverageV1 {
    visible: u64,
    hidden: u64,
    unknown: u64,
    redacted: u64,
}

impl FactQueryCoverageV1 {
    pub const fn new(visible: u64, hidden: u64, unknown: u64, redacted: u64) -> Self {
        Self {
            visible,
            hidden,
            unknown,
            redacted,
        }
    }

    pub const fn visible(&self) -> u64 {
        self.visible
    }

    pub const fn hidden(&self) -> u64 {
        self.hidden
    }

    pub const fn unknown(&self) -> u64 {
        self.unknown
    }

    pub const fn redacted(&self) -> u64 {
        self.redacted
    }
}

/// Explicit contradiction knowledge at the response snapshot.
///
/// Positive identifiers are sorted, deduplicated, and bounded by
/// [`MAX_FACT_QUERY_CONTRADICTIONS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactContradictionStateV1 {
    Unknown,
    NotObserved,
    Present { contradicted_by: Vec<FactId> },
}

impl FactContradictionStateV1 {
    pub fn from_positive(mut contradicted_by: Vec<FactId>) -> Self {
        contradicted_by.sort_unstable();
        contradicted_by.dedup();
        contradicted_by.truncate(MAX_FACT_QUERY_CONTRADICTIONS);
        if contradicted_by.is_empty() {
            Self::NotObserved
        } else {
            Self::Present { contradicted_by }
        }
    }

    pub fn contradicted_by(&self) -> &[FactId] {
        match self {
            Self::Present { contradicted_by } => contradicted_by,
            Self::Unknown | Self::NotObserved => &[],
        }
    }

    pub const fn is_positive(&self) -> bool {
        matches!(self, Self::Present { .. })
    }
}

/// Current fact projection plus explicit coverage and contradiction state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCurrentResponseV1 {
    fact: Option<StoredFactV1>,
    coverage: FactQueryCoverageV1,
    contradiction: FactContradictionStateV1,
}

impl FactCurrentResponseV1 {
    pub fn new(
        fact: Option<StoredFactV1>,
        coverage: FactQueryCoverageV1,
        contradiction: FactContradictionStateV1,
    ) -> Self {
        Self {
            fact,
            coverage,
            contradiction,
        }
    }

    pub fn fact(&self) -> Option<&StoredFactV1> {
        self.fact.as_ref()
    }

    pub const fn coverage(&self) -> &FactQueryCoverageV1 {
        &self.coverage
    }

    pub const fn contradiction(&self) -> &FactContradictionStateV1 {
        &self.contradiction
    }
}

/// As-of fact projection plus explicit coverage and contradiction state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAsOfResponseV1 {
    fact: Option<StoredFactV1>,
    coverage: FactQueryCoverageV1,
    contradiction: FactContradictionStateV1,
}

impl FactAsOfResponseV1 {
    pub fn new(
        fact: Option<StoredFactV1>,
        coverage: FactQueryCoverageV1,
        contradiction: FactContradictionStateV1,
    ) -> Self {
        Self {
            fact,
            coverage,
            contradiction,
        }
    }

    pub fn fact(&self) -> Option<&StoredFactV1> {
        self.fact.as_ref()
    }

    pub const fn coverage(&self) -> &FactQueryCoverageV1 {
        &self.coverage
    }

    pub const fn contradiction(&self) -> &FactContradictionStateV1 {
        &self.contradiction
    }
}

/// Bounded lineage page plus snapshot-wide coverage and contradiction state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactLineageResponseV1 {
    events: Vec<FactLineageEventV1>,
    coverage: FactQueryCoverageV1,
    contradiction: FactContradictionStateV1,
}

impl FactLineageResponseV1 {
    pub fn new(
        events: Vec<FactLineageEventV1>,
        coverage: FactQueryCoverageV1,
        contradiction: FactContradictionStateV1,
    ) -> Self {
        Self {
            events,
            coverage,
            contradiction,
        }
    }

    pub fn events(&self) -> &[FactLineageEventV1] {
        &self.events
    }

    pub const fn coverage(&self) -> &FactQueryCoverageV1 {
        &self.coverage
    }

    pub const fn contradiction(&self) -> &FactContradictionStateV1 {
        &self.contradiction
    }
}

/// Page of current facts ordered by `(FactId)` after the exclusive cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentFactsQuery {
    owner: FactOwnerV1,
    after_fact_id: Option<FactId>,
    limit: usize,
}

/// One current fact, authorized by its canonical owner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactCurrentQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
}

impl FactCurrentQuery {
    pub fn new(owner: FactOwnerV1, fact_id: FactId) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self { owner, fact_id })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
}

impl CurrentFactsQuery {
    pub fn new(
        owner: FactOwnerV1,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &after_fact_id {
            fact_id.validate()?;
            validate_owned_fact_id(fact_id, &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            after_fact_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn after_fact_id(&self) -> Option<&FactId> {
        self.after_fact_id.as_ref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// One fact projected through an inclusive UTC timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAsOfQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
    as_of: UtcMicros,
}

impl FactAsOfQuery {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, as_of: UtcMicros) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self {
            owner,
            fact_id,
            as_of,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn as_of(&self) -> UtcMicros {
        self.as_of
    }
}

/// Exclusive cursor for lineage ordered by `(occurred_at, FactEventId)`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactLineageCursor {
    occurred_at: UtcMicros,
    event_id: FactEventId,
}

impl FactLineageCursor {
    pub fn new(occurred_at: UtcMicros, event_id: FactEventId) -> FactStoreResult<Self> {
        event_id.validate()?;
        Ok(Self {
            occurred_at,
            event_id,
        })
    }

    pub fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
}

/// Page of lineage events ordered by `(occurred_at, FactEventId)`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactLineageQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
    after: Option<FactLineageCursor>,
    limit: usize,
}

/// Compatibility lookup for one V1 integer identity in its original store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyFactQuery {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    legacy_fact_id: i64,
}

impl LegacyFactQuery {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        legacy_fact_id: i64,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        source_store_id.validate()?;
        if legacy_fact_id <= 0 {
            return Err(FactStoreError::InvalidLegacyFactId { legacy_fact_id });
        }
        Ok(Self {
            owner,
            source_store_id,
            legacy_fact_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }

    pub fn legacy_fact_id(&self) -> i64 {
        self.legacy_fact_id
    }

    /// Validate the canonical result returned for this legacy lookup.
    pub fn validate_resolved_fact_id(&self, fact_id: &FactId) -> FactStoreResult<()> {
        validate_owned_fact_id(fact_id, &self.owner)
    }
}

impl FactLineageQuery {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        after: Option<FactLineageCursor>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            owner,
            fact_id,
            after,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Owner-authorized lookup for a stable retrieval anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalAnchorQuery {
    owner: FactOwnerV1,
    anchor_id: RetrievalAnchorId,
}

impl RetrievalAnchorQuery {
    pub fn new(owner: FactOwnerV1, anchor_id: RetrievalAnchorId) -> FactStoreResult<Self> {
        owner.validate()?;
        anchor_id.validate()?;
        Ok(Self { owner, anchor_id })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }
}

pub(super) fn validate_limit(limit: usize, max: usize) -> FactStoreResult<()> {
    if !(1..=max).contains(&limit) {
        return Err(FactStoreError::InvalidQueryLimit { limit, max });
    }
    Ok(())
}

/// Owner-bound exact-content lookup for proposal validation. The digest is
/// derived at the application boundary from sanitized content; storage never
/// accepts a raw proposal payload for this read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactContentDigestQueryV1 {
    owner: FactOwnerV1,
    content_digest: LocatorDigest,
}

impl ProjectMemoryFactContentDigestQueryV1 {
    pub fn new(owner: FactOwnerV1, content_digest: LocatorDigest) -> FactStoreResult<Self> {
        owner.validate()?;
        content_digest.validate()?;
        Ok(Self {
            owner,
            content_digest,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn content_digest(&self) -> &LocatorDigest {
        &self.content_digest
    }
}

/// Bounded request for search, probe, related, or reason retrieval.  Search
/// results must use deterministic score/fact-ID ordering in the response DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactSearchQuery {
    owner: FactOwnerV1,
    kind: ProjectMemoryFactSearchKindV1,
    query: Option<String>,
    filter: ProjectMemoryFactSearchFilterV1,
    after: Option<ProjectMemoryFactSearchCursorV1>,
    limit: usize,
}

impl ProjectMemoryFactSearchQuery {
    pub fn new(
        owner: FactOwnerV1,
        kind: ProjectMemoryFactSearchKindV1,
        query: Option<String>,
        after: Option<ProjectMemoryFactSearchCursorV1>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        Self::with_filter(
            owner,
            kind,
            query,
            ProjectMemoryFactSearchFilterV1::default(),
            after,
            limit,
        )
    }

    pub fn with_filter(
        owner: FactOwnerV1,
        kind: ProjectMemoryFactSearchKindV1,
        query: Option<String>,
        filter: ProjectMemoryFactSearchFilterV1,
        after: Option<ProjectMemoryFactSearchCursorV1>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        kind.validate()?;
        if let Some(query) = &query {
            if query.trim().is_empty() || query.len() > MAX_COMPATIBILITY_SEARCH_BYTES {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search query",
                }));
            }
        } else if matches!(
            &kind,
            ProjectMemoryFactSearchKindV1::Search | ProjectMemoryFactSearchKindV1::Probe
        ) {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "compatibility fact search query",
            }));
        }
        if let Some(cursor) = &after {
            validate_owned_fact_id(cursor.fact_id(), &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            kind,
            query,
            filter,
            after,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn kind(&self) -> ProjectMemoryFactSearchKindV1 {
        self.kind.clone()
    }
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
    pub fn filter(&self) -> &ProjectMemoryFactSearchFilterV1 {
        &self.filter
    }
    pub fn after(&self) -> Option<&ProjectMemoryFactSearchCursorV1> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Deterministic compatibility list filters without exposing raw SQL fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactListQueryV1 {
    owner: FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    after_fact_id: Option<FactId>,
    limit: usize,
}

impl ProjectMemoryFactListQueryV1 {
    pub fn new(
        owner: FactOwnerV1,
        category: Option<FactCategoryV1>,
        min_trust: Option<Confidence>,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &after_fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            category,
            min_trust,
            after_fact_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn min_trust(&self) -> Option<Confidence> {
        self.min_trust
    }
    pub fn after_fact_id(&self) -> Option<&FactId> {
        self.after_fact_id.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactHistoryQueryV1 {
    target: ProjectMemoryFactTargetV1,
    after: Option<FactLineageCursor>,
    limit: usize,
}

impl ProjectMemoryFactHistoryQueryV1 {
    pub fn new(
        target: ProjectMemoryFactTargetV1,
        after: Option<FactLineageCursor>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            target,
            after,
            limit,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactTargetV1 {
        &self.target
    }
    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactFeedbackHistoryQueryV1 {
    target: ProjectMemoryFactTargetV1,
    after: Option<FactLineageCursor>,
    limit: usize,
}

impl ProjectMemoryFactFeedbackHistoryQueryV1 {
    pub fn new(
        target: ProjectMemoryFactTargetV1,
        after: Option<FactLineageCursor>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            target,
            after,
            limit,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactTargetV1 {
        &self.target
    }
    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}
