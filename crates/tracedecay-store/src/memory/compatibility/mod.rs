use serde_json::Value;
use tracedecay_domain::canonical_text::is_canonical_text_within;
use tracedecay_domain::{
    DomainError, FactCategoryV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1,
    RetrievalAnchorId, RetrievalAnchorRecordV2,
};

use super::queries::{MAX_CURRENT_LIMIT, MAX_LINEAGE_LIMIT};
use super::{
    FactLineageCursor, FactLineageError, FactLineageResult, FactStatus, FactTelemetry,
    LegacyFactQuery, MAX_FACT_REASON_BYTES, MAX_FACT_SEARCH_BYTES, StoredFactV1,
    validate_owned_fact_id,
};

mod curation;
pub(super) mod dashboard;
mod proposal;
mod search;

pub use curation::{
    FactAddAlias, FactAddCommand, FactAddDisposition, FactAddOutcome, FactCurationBatch,
    FactCurationOperation, FactCurationReceipt, FactEntityTarget, FactFeedbackCommand,
    FactFeedbackOutcome, FactLink, FactMergeCommand, FactMergeEntities, FactMergeOutcome,
    FactNormalizeTags, FactRelation, FactRemoveCommand, FactRemoveOutcome, FactRepairVector,
    FactUpdateCommand, FactUpdateOutcome, FactUpdatePatch, MemoryRepairCommand,
};
pub use dashboard::{
    DashboardEntity, DashboardFactDetail, DashboardFactDetailQuery, DashboardFactEntityLink,
    DashboardFactSummary, DashboardGrowthPoint, DashboardHrrCoverage, DashboardHrrState,
    DashboardMemoryBank, DashboardMemoryOverview, DashboardMemoryOverviewQuery,
    DashboardNamedCount, DashboardOplogDetails, DashboardOplogEntry, DashboardOplogQuery,
    DashboardVectorPoint, DashboardVectorPointsQuery,
};
pub use proposal::{
    FactProposalEvidence, FactProposalPage, FactProposalPromotion,
    FactProposalPromotionDisposition, FactProposalPromotionResult, FactProposalPromotionStateV1,
    FactProposalRecord, FactProposalRevision, FactProposalState, PromoteFactProposal,
    PromoteFactProposalOutcome,
};
pub use search::{
    FactContradiction, FactContradictionPage, FactContradictionQuery, FactRetrievalCommand,
    FactSearchCursor, FactSearchFilter, FactSearchHit, FactSearchKind, FactSearchPage,
    FactSearchScores,
};

fn validate_entity(value: &str) -> FactLineageResult<()> {
    validate_text(value, "compatibility fact entity")
}

fn validate_text(value: &str, field: &'static str) -> FactLineageResult<()> {
    if !is_canonical_text_within(value, MAX_FACT_SEARCH_BYTES) {
        return Err(FactLineageError::Contract(DomainError::NonCanonical {
            field,
        }));
    }
    Ok(())
}

fn validate_metadata(value: &Value, field: &'static str) -> FactLineageResult<()> {
    if serde_json::to_vec(value)
        .map(|encoded| encoded.len() > MAX_FACT_SEARCH_BYTES)
        .unwrap_or(true)
    {
        return Err(FactLineageError::Contract(DomainError::NonCanonical {
            field,
        }));
    }
    Ok(())
}

/// Stable, owner-bound identifier used by V1-compatible fact surfaces.  It is
/// deliberately the canonical fact identity rather than a process-local row
/// number; an optional [`LegacyFactMappingV1`] carries a historical `i64` only
/// where the authoritative migration reconstructed one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnedFactId {
    owner: FactOwnerV1,
    fact_id: FactId,
}

impl OwnedFactId {
    pub fn new(owner: FactOwnerV1, fact_id: FactId) -> FactLineageResult<Self> {
        owner.validate()?;
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

/// Owner-bound forward/reverse compatibility mapping.  The optional legacy
/// mapping is the sole source of a legacy integer identifier; callers must not
/// coerce or hash canonical identifiers into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactMapping {
    id: OwnedFactId,
    legacy_mapping: Option<LegacyFactMappingV1>,
}

impl FactMapping {
    pub fn new(
        id: OwnedFactId,
        legacy_mapping: Option<LegacyFactMappingV1>,
    ) -> FactLineageResult<Self> {
        if let Some(mapping) = &legacy_mapping {
            if mapping.owner() != id.owner() {
                return Err(FactLineageError::OwnerMismatch);
            }
            if mapping.fact_id() != id.fact_id() {
                return Err(FactLineageError::FactMismatch);
            }
        }
        Ok(Self { id, legacy_mapping })
    }

    pub fn id(&self) -> &OwnedFactId {
        &self.id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.id.owner()
    }

    pub fn fact_id(&self) -> &FactId {
        self.id.fact_id()
    }

    pub fn legacy_mapping(&self) -> Option<&LegacyFactMappingV1> {
        self.legacy_mapping.as_ref()
    }

    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.legacy_mapping
            .as_ref()
            .map(LegacyFactMappingV1::legacy_fact_id)
    }

    pub fn history_coverage(&self) -> Option<LegacyHistoryCoverageV1> {
        self.legacy_mapping
            .as_ref()
            .map(LegacyFactMappingV1::history_coverage)
    }
}

/// Typed source provenance for a compatibility projection.  Canonical sources
/// contain only sanitized domain identifiers; `Unknown` is explicit for legacy
/// history that cannot be reconstructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactSource {
    Canonical(FactIdentitySourceV1),
    Unknown,
}

impl FactSource {
    fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactLineageResult<()> {
        if let Self::Canonical(source) = self {
            FactIdentityMaterialV1::new(owner.clone(), source.clone())?;
        }
        Ok(())
    }
}

/// V1-shaped projection of one canonical fact.  `StoredFactV1` keeps access
/// state and the sanitized [`FactPayloadV1`] together so adapters cannot expose
/// deleted or un-sanitized payload fields accidentally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    fact: StoredFactV1,
    mapping: FactMapping,
    source: FactSource,
    source_label: Option<String>,
    telemetry: FactTelemetry,
}

impl Fact {
    pub fn new(
        fact: StoredFactV1,
        mapping: FactMapping,
        source: FactSource,
        telemetry: FactTelemetry,
    ) -> FactLineageResult<Self> {
        if fact.owner() != mapping.owner() {
            return Err(FactLineageError::OwnerMismatch);
        }
        if fact.fact_id() != mapping.fact_id() {
            return Err(FactLineageError::FactMismatch);
        }
        if fact
            .legacy_mapping()
            .is_some_and(|legacy| mapping.legacy_mapping() != Some(legacy))
        {
            return Err(FactLineageError::FactMismatch);
        }
        source.validate_for_owner(fact.owner())?;
        if let FactSource::Canonical(identity_source) = &source {
            let material =
                FactIdentityMaterialV1::new(fact.owner().clone(), identity_source.clone())?;
            if FactId::derive(&material)? != *fact.fact_id() {
                return Err(FactLineageError::FactMismatch);
            }
        }
        Ok(Self {
            fact,
            mapping,
            source,
            source_label: None,
            telemetry,
        })
    }

    pub fn with_source_label(mut self, source_label: Option<String>) -> FactLineageResult<Self> {
        if source_label
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact source label",
            }));
        }
        self.source_label = source_label;
        Ok(self)
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactLineageResult<()> {
        if self.owner() != owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(())
    }

    pub fn fact(&self) -> &StoredFactV1 {
        &self.fact
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        self.fact.owner()
    }
    pub fn fact_id(&self) -> &FactId {
        self.fact.fact_id()
    }
    pub fn mapping(&self) -> &FactMapping {
        &self.mapping
    }
    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.mapping.legacy_fact_id()
    }
    pub fn source(&self) -> &FactSource {
        &self.source
    }
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn telemetry(&self) -> &FactTelemetry {
        &self.telemetry
    }
    pub fn payload(&self) -> Option<&FactPayloadV1> {
        self.fact.payload()
    }
    pub fn content(&self) -> Option<&str> {
        self.payload().map(FactPayloadV1::content)
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.payload().map(FactPayloadV1::category)
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.payload().map(FactPayloadV1::tags)
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.payload().map(FactPayloadV1::entities)
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.payload().map(FactPayloadV1::metadata)
    }
}

/// A bounded, deterministic compatibility list page.  Facts are sorted by
/// canonical `FactId` ascending, which makes the cursor stable across rebuilds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactPage {
    owner: FactOwnerV1,
    facts: Vec<FactProjection>,
    next_after_fact_id: Option<FactId>,
}

impl FactPage {
    pub fn new(
        owner: FactOwnerV1,
        facts: Vec<FactProjection>,
        next_after_fact_id: Option<FactId>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        if facts.len() > MAX_CURRENT_LIMIT {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: facts.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&FactId> = None;
        for fact in &facts {
            if fact.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= fact.fact_id()) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact page order",
                }));
            }
            previous = Some(fact.fact_id());
        }
        if let Some(cursor) = &next_after_fact_id {
            validate_owned_fact_id(cursor, &owner)?;
            // Resume semantics are exclusive-start (`fact_id > cursor`), so
            // the canonical cursor for a full page is exactly its last fact
            // id — the same convention the search-page cursor uses. Anything
            // else either re-serves returned rows or silently skips rows.
            if previous != Some(cursor) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact page cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            facts,
            next_after_fact_id,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactLineageResult<()> {
        if &self.owner != owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(())
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn facts(&self) -> &[FactProjection] {
        &self.facts
    }
    pub fn next_after_fact_id(&self) -> Option<&FactId> {
        self.next_after_fact_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactHistory {
    owner: FactOwnerV1,
    fact_id: FactId,
    events: Vec<FactLineageEventV1>,
    next_after: Option<FactLineageCursor>,
}

impl FactHistory {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        events: Vec<FactLineageEventV1>,
        next_after: Option<FactLineageCursor>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        if events.len() > MAX_LINEAGE_LIMIT {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: events.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&FactLineageEventV1> = None;
        for event in &events {
            if event.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
            if event.fact_id() != &fact_id {
                return Err(FactLineageError::FactMismatch);
            }
            if previous.is_some_and(|value| {
                (value.occurred_at(), value.event_id()) >= (event.occurred_at(), event.event_id())
            }) {
                return Err(FactLineageError::EventsOutOfOrder);
            }
            previous = Some(event);
        }
        Ok(Self {
            owner,
            fact_id,
            events,
            next_after,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactLineageResult<()> {
        if &self.owner != owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(())
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
    pub fn events(&self) -> &[FactLineageEventV1] {
        &self.events
    }
    pub fn next_after(&self) -> Option<&FactLineageCursor> {
        self.next_after.as_ref()
    }
}

/// Bounded detail projection used for V1 `get`, history, status, and dashboard
/// inspection without exposing a database row or arbitrary JSON transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactInspection {
    fact: Fact,
    history: FactHistory,
    anchors: Vec<RetrievalAnchorRecordV2>,
    status: FactStatus,
}

impl FactInspection {
    pub fn new(
        fact: Fact,
        history: FactHistory,
        anchors: Vec<RetrievalAnchorRecordV2>,
        status: FactStatus,
    ) -> FactLineageResult<Self> {
        history.validate_for_owner(fact.owner())?;
        status.validate_for_owner(fact.owner())?;
        if history.fact_id() != fact.fact_id()
            || status.fact_id().is_some_and(|id| id != fact.fact_id())
        {
            return Err(FactLineageError::FactMismatch);
        }
        if anchors.len() > MAX_LINEAGE_LIMIT {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: anchors.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&RetrievalAnchorId> = None;
        for anchor in &anchors {
            anchor.validate()?;
            if FactOwnerV1::from(anchor.owner().clone()) != *fact.owner() {
                return Err(FactLineageError::OwnerMismatch);
            }
            if previous.is_some_and(|id| id >= anchor.anchor_id()) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact inspection anchors",
                }));
            }
            previous = Some(anchor.anchor_id());
        }
        Ok(Self {
            fact,
            history,
            anchors,
            status,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactLineageResult<()> {
        self.fact.validate_for_owner(owner)
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        self.fact.owner()
    }
    pub fn fact(&self) -> &Fact {
        &self.fact
    }
    pub fn history(&self) -> &FactHistory {
        &self.history
    }
    pub fn anchors(&self) -> &[RetrievalAnchorRecordV2] {
        &self.anchors
    }
    pub fn status(&self) -> &FactStatus {
        &self.status
    }
}

/// A compatibility operation may target a canonical fact or an owner-bound
/// historical numeric identity.  Resolution of the latter happens inside the
/// authority transaction, never in a handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactTarget {
    Canonical(OwnedFactId),
    Legacy(LegacyFactQuery),
}

impl FactTarget {
    fn validate(&self) -> FactLineageResult<()> {
        match self {
            Self::Canonical(target) => {
                target.owner().validate()?;
                validate_owned_fact_id(target.fact_id(), target.owner())
            }
            Self::Legacy(target) => {
                target.owner().validate()?;
                target.source_store_id().validate()?;
                if target.legacy_fact_id() <= 0 {
                    return Err(FactLineageError::InvalidLegacyFactId {
                        legacy_fact_id: target.legacy_fact_id(),
                    });
                }
                Ok(())
            }
        }
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Canonical(target) => target.owner(),
            Self::Legacy(target) => target.owner(),
        }
    }

    pub fn canonical_fact_id(&self) -> Option<&FactId> {
        match self {
            Self::Canonical(target) => Some(target.fact_id()),
            Self::Legacy(_) => None,
        }
    }

    pub fn legacy_query(&self) -> Option<&LegacyFactQuery> {
        match self {
            Self::Canonical(_) => None,
            Self::Legacy(target) => Some(target),
        }
    }
}

/// Safe representation for a migrated or deleted fact that cannot satisfy the
/// canonical active-assertion invariant of [`StoredFactV1`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactAvailability {
    Deleted,
    Quarantined,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactUnavailable {
    target: OwnedFactId,
    availability: FactAvailability,
    status: FactStatus,
}

impl FactUnavailable {
    pub fn new(
        target: OwnedFactId,
        availability: FactAvailability,
        status: FactStatus,
    ) -> FactLineageResult<Self> {
        status.validate_for_owner(target.owner())?;
        if status
            .fact_id()
            .is_some_and(|fact_id| fact_id != target.fact_id())
        {
            return Err(FactLineageError::FactMismatch);
        }
        Ok(Self {
            target,
            availability,
            status,
        })
    }

    pub fn target(&self) -> &OwnedFactId {
        &self.target
    }
    pub fn availability(&self) -> FactAvailability {
        self.availability
    }
    pub fn status(&self) -> &FactStatus {
        &self.status
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactProjection {
    Available(Box<Fact>),
    Unavailable(FactUnavailable),
}

impl FactProjection {
    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Available(fact) => fact.owner(),
            Self::Unavailable(fact) => fact.target().owner(),
        }
    }

    pub fn fact_id(&self) -> &FactId {
        match self {
            Self::Available(fact) => fact.fact_id(),
            Self::Unavailable(fact) => fact.target().fact_id(),
        }
    }

    pub fn mapping(&self) -> Option<&FactMapping> {
        match self {
            Self::Available(fact) => Some(fact.mapping()),
            Self::Unavailable(_) => None,
        }
    }
}
