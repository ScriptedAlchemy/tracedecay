use tracedecay_domain::{
    Confidence, DomainError, FactCategoryV1, FactEventId, FactId, FactOwnerV1, LocatorDigest,
    RetrievalAnchorId, SourceStoreId, UtcMicros,
};

use super::{
    CompatibilityFactSearchCursorV1, CompatibilityFactSearchFilterV1,
    CompatibilityFactSearchKindV1, CompatibilityFactTargetV1, FactStoreError, FactStoreResult,
    MAX_COMPATIBILITY_SEARCH_BYTES, validate_owned_fact_id,
};

pub(super) const MAX_CURRENT_LIMIT: usize = 1_000;

pub(super) const MAX_LINEAGE_LIMIT: usize = 1_000;

/// Page of current facts ordered by `(FactId)` after the exclusive cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentFactsQuery {
    owner: FactOwnerV1,
    after_fact_id: Option<FactId>,
    limit: usize,
}

/// One current fact, authorized by its canonical owner.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
pub struct CompatibilityFactContentDigestQueryV1 {
    owner: FactOwnerV1,
    content_digest: LocatorDigest,
}

impl CompatibilityFactContentDigestQueryV1 {
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
pub struct CompatibilityFactSearchQuery {
    owner: FactOwnerV1,
    kind: CompatibilityFactSearchKindV1,
    query: Option<String>,
    filter: CompatibilityFactSearchFilterV1,
    after: Option<CompatibilityFactSearchCursorV1>,
    limit: usize,
}

impl CompatibilityFactSearchQuery {
    pub fn new(
        owner: FactOwnerV1,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        after: Option<CompatibilityFactSearchCursorV1>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        Self::with_filter(
            owner,
            kind,
            query,
            CompatibilityFactSearchFilterV1::default(),
            after,
            limit,
        )
    }

    pub fn with_filter(
        owner: FactOwnerV1,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        filter: CompatibilityFactSearchFilterV1,
        after: Option<CompatibilityFactSearchCursorV1>,
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
            CompatibilityFactSearchKindV1::Search | CompatibilityFactSearchKindV1::Probe
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
    pub fn kind(&self) -> CompatibilityFactSearchKindV1 {
        self.kind.clone()
    }
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
    pub fn filter(&self) -> &CompatibilityFactSearchFilterV1 {
        &self.filter
    }
    pub fn after(&self) -> Option<&CompatibilityFactSearchCursorV1> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Deterministic compatibility list filters without exposing raw SQL fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactListQueryV1 {
    owner: FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    after_fact_id: Option<FactId>,
    limit: usize,
}

impl CompatibilityFactListQueryV1 {
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
pub struct CompatibilityFactHistoryQueryV1 {
    target: CompatibilityFactTargetV1,
    after: Option<FactLineageCursor>,
    limit: usize,
}

impl CompatibilityFactHistoryQueryV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
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

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
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
pub struct CompatibilityFactFeedbackHistoryQueryV1 {
    target: CompatibilityFactTargetV1,
    after: Option<FactLineageCursor>,
    limit: usize,
}

impl CompatibilityFactFeedbackHistoryQueryV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
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

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}
