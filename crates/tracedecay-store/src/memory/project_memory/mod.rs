use serde_json::Value;
use tracedecay_domain::canonical_text::is_canonical_text_within;
use tracedecay_domain::{
    DomainError, FactCategoryV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1,
    RetrievalAnchorId, RetrievalAnchorRecordV2,
};

use super::queries::{MAX_CURRENT_LIMIT, MAX_LINEAGE_LIMIT};
use super::{
    ProjectMemoryFactStatusV1, ProjectMemoryFactTelemetryV1, FactLineageCursor, FactStoreError,
    FactStoreResult, LegacyFactQuery, MAX_COMPATIBILITY_REASON_BYTES,
    MAX_COMPATIBILITY_SEARCH_BYTES, StoredFactV1, validate_owned_fact_id,
};

mod curation;
pub(super) mod dashboard;
mod proposal;
mod search;

pub use curation::{
    ProjectMemoryFactAddAliasV1, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactLinkV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeEntitiesV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactNormalizeTagsV1, ProjectMemoryFactRelationV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRepairVectorV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryFactUpdatePatchV1,
    ProjectMemoryLegacyEntityTargetV1, ProjectMemoryMemoryRepairCommandV1,
    ProjectMemoryRelationProvenanceV1,
};
pub use dashboard::{
    ProjectMemoryDashboardEntityV1, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardFactEntityLinkV1,
    ProjectMemoryDashboardFactSummaryV1, ProjectMemoryDashboardGrowthPointV1,
    ProjectMemoryDashboardHrrCoverageV1, ProjectMemoryDashboardHrrStateV1,
    ProjectMemoryDashboardMemoryBankV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardNamedCountV1,
    ProjectMemoryDashboardOplogDetailsV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1,
};
pub use proposal::{
    ProjectMemoryFactProposalImportReceiptV1, ProjectMemoryFactProposalImportV1,
    ProjectMemoryFactProposalLegacyRecordV1, ProjectMemoryFactProposalPageV1,
    ProjectMemoryFactProposalPromotionDispositionV1, ProjectMemoryFactProposalPromotionResultV1,
    ProjectMemoryFactProposalPromotionV1, ProjectMemoryFactProposalRecordV1,
    ProjectMemoryFactProposalRevisionV1, ProjectMemoryFactProposalStateV1,
    FactProposalPromotionStateV1, PromoteFactProposal, PromoteFactProposalOutcome,
};
pub use search::{
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactContradictionV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchHitV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchScoresV1,
};

fn validate_project_memory_entity(value: &str) -> FactStoreResult<()> {
    validate_compatibility_text(value, "compatibility fact entity")
}

fn validate_compatibility_text(value: &str, field: &'static str) -> FactStoreResult<()> {
    if !is_canonical_text_within(value, MAX_COMPATIBILITY_SEARCH_BYTES) {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field,
        }));
    }
    Ok(())
}

fn validate_compatibility_metadata(value: &Value, field: &'static str) -> FactStoreResult<()> {
    if serde_json::to_vec(value)
        .map(|encoded| encoded.len() > MAX_COMPATIBILITY_SEARCH_BYTES)
        .unwrap_or(true)
    {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
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
pub struct ProjectMemoryFactIdV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
}

impl ProjectMemoryFactIdV1 {
    pub fn new(owner: FactOwnerV1, fact_id: FactId) -> FactStoreResult<Self> {
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
pub struct ProjectMemoryFactMappingV1 {
    compatibility_id: ProjectMemoryFactIdV1,
    legacy_mapping: Option<LegacyFactMappingV1>,
}

impl ProjectMemoryFactMappingV1 {
    pub fn new(
        compatibility_id: ProjectMemoryFactIdV1,
        legacy_mapping: Option<LegacyFactMappingV1>,
    ) -> FactStoreResult<Self> {
        if let Some(mapping) = &legacy_mapping {
            if mapping.owner() != compatibility_id.owner() {
                return Err(FactStoreError::OwnerMismatch);
            }
            if mapping.fact_id() != compatibility_id.fact_id() {
                return Err(FactStoreError::FactMismatch);
            }
        }
        Ok(Self {
            compatibility_id,
            legacy_mapping,
        })
    }

    pub fn compatibility_id(&self) -> &ProjectMemoryFactIdV1 {
        &self.compatibility_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.compatibility_id.owner()
    }

    pub fn fact_id(&self) -> &FactId {
        self.compatibility_id.fact_id()
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
pub enum ProjectMemoryFactSourceV1 {
    Canonical(FactIdentitySourceV1),
    Unknown,
}

impl ProjectMemoryFactSourceV1 {
    fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
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
pub struct ProjectMemoryFactV1 {
    fact: StoredFactV1,
    mapping: ProjectMemoryFactMappingV1,
    source: ProjectMemoryFactSourceV1,
    source_label: Option<String>,
    telemetry: ProjectMemoryFactTelemetryV1,
}

impl ProjectMemoryFactV1 {
    pub fn new(
        fact: StoredFactV1,
        mapping: ProjectMemoryFactMappingV1,
        source: ProjectMemoryFactSourceV1,
        telemetry: ProjectMemoryFactTelemetryV1,
    ) -> FactStoreResult<Self> {
        if fact.owner() != mapping.owner() {
            return Err(FactStoreError::OwnerMismatch);
        }
        if fact.fact_id() != mapping.fact_id() {
            return Err(FactStoreError::FactMismatch);
        }
        if fact
            .legacy_mapping()
            .is_some_and(|legacy| mapping.legacy_mapping() != Some(legacy))
        {
            return Err(FactStoreError::FactMismatch);
        }
        source.validate_for_owner(fact.owner())?;
        if let ProjectMemoryFactSourceV1::Canonical(identity_source) = &source {
            let material =
                FactIdentityMaterialV1::new(fact.owner().clone(), identity_source.clone())?;
            if FactId::derive(&material)? != *fact.fact_id() {
                return Err(FactStoreError::FactMismatch);
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

    pub fn with_source_label(mut self, source_label: Option<String>) -> FactStoreResult<Self> {
        if source_label.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact source label",
            }));
        }
        self.source_label = source_label;
        Ok(self)
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if self.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
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
    pub fn mapping(&self) -> &ProjectMemoryFactMappingV1 {
        &self.mapping
    }
    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.mapping.legacy_fact_id()
    }
    pub fn source(&self) -> &ProjectMemoryFactSourceV1 {
        &self.source
    }
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn telemetry(&self) -> &ProjectMemoryFactTelemetryV1 {
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
pub struct ProjectMemoryFactPageV1 {
    owner: FactOwnerV1,
    facts: Vec<ProjectMemoryFactProjectionV1>,
    next_after_fact_id: Option<FactId>,
}

impl ProjectMemoryFactPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        facts: Vec<ProjectMemoryFactProjectionV1>,
        next_after_fact_id: Option<FactId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if facts.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: facts.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&FactId> = None;
        for fact in &facts {
            if fact.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= fact.fact_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
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
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
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

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn facts(&self) -> &[ProjectMemoryFactProjectionV1] {
        &self.facts
    }
    pub fn next_after_fact_id(&self) -> Option<&FactId> {
        self.next_after_fact_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactHistoryV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
    events: Vec<FactLineageEventV1>,
    next_after: Option<FactLineageCursor>,
}

impl ProjectMemoryFactHistoryV1 {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        events: Vec<FactLineageEventV1>,
        next_after: Option<FactLineageCursor>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        if events.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: events.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&FactLineageEventV1> = None;
        for event in &events {
            if event.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if event.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if previous.is_some_and(|value| {
                (value.occurred_at(), value.event_id()) >= (event.occurred_at(), event.event_id())
            }) {
                return Err(FactStoreError::EventsOutOfOrder);
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

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
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
pub struct ProjectMemoryFactInspectionV1 {
    fact: ProjectMemoryFactV1,
    history: ProjectMemoryFactHistoryV1,
    anchors: Vec<RetrievalAnchorRecordV2>,
    status: ProjectMemoryFactStatusV1,
}

impl ProjectMemoryFactInspectionV1 {
    pub fn new(
        fact: ProjectMemoryFactV1,
        history: ProjectMemoryFactHistoryV1,
        anchors: Vec<RetrievalAnchorRecordV2>,
        status: ProjectMemoryFactStatusV1,
    ) -> FactStoreResult<Self> {
        history.validate_for_owner(fact.owner())?;
        status.validate_for_owner(fact.owner())?;
        if history.fact_id() != fact.fact_id()
            || status.fact_id().is_some_and(|id| id != fact.fact_id())
        {
            return Err(FactStoreError::FactMismatch);
        }
        if anchors.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: anchors.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&RetrievalAnchorId> = None;
        for anchor in &anchors {
            anchor.validate()?;
            if FactOwnerV1::from(anchor.owner().clone()) != *fact.owner() {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|id| id >= anchor.anchor_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
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

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        self.fact.validate_for_owner(owner)
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        self.fact.owner()
    }
    pub fn fact(&self) -> &ProjectMemoryFactV1 {
        &self.fact
    }
    pub fn history(&self) -> &ProjectMemoryFactHistoryV1 {
        &self.history
    }
    pub fn anchors(&self) -> &[RetrievalAnchorRecordV2] {
        &self.anchors
    }
    pub fn status(&self) -> &ProjectMemoryFactStatusV1 {
        &self.status
    }
}

/// A compatibility operation may target a canonical fact or an owner-bound
/// historical numeric identity.  Resolution of the latter happens inside the
/// authority transaction, never in a handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactTargetV1 {
    Canonical(ProjectMemoryFactIdV1),
    Legacy(LegacyFactQuery),
}

impl ProjectMemoryFactTargetV1 {
    fn validate(&self) -> FactStoreResult<()> {
        match self {
            Self::Canonical(target) => {
                target.owner().validate()?;
                validate_owned_fact_id(target.fact_id(), target.owner())
            }
            Self::Legacy(target) => {
                target.owner().validate()?;
                target.source_store_id().validate()?;
                if target.legacy_fact_id() <= 0 {
                    return Err(FactStoreError::InvalidLegacyFactId {
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
pub enum ProjectMemoryFactAvailabilityV1 {
    Deleted,
    Quarantined,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactUnavailableV1 {
    target: ProjectMemoryFactIdV1,
    availability: ProjectMemoryFactAvailabilityV1,
    status: ProjectMemoryFactStatusV1,
}

impl ProjectMemoryFactUnavailableV1 {
    pub fn new(
        target: ProjectMemoryFactIdV1,
        availability: ProjectMemoryFactAvailabilityV1,
        status: ProjectMemoryFactStatusV1,
    ) -> FactStoreResult<Self> {
        status.validate_for_owner(target.owner())?;
        if status
            .fact_id()
            .is_some_and(|fact_id| fact_id != target.fact_id())
        {
            return Err(FactStoreError::FactMismatch);
        }
        Ok(Self {
            target,
            availability,
            status,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactIdV1 {
        &self.target
    }
    pub fn availability(&self) -> ProjectMemoryFactAvailabilityV1 {
        self.availability
    }
    pub fn status(&self) -> &ProjectMemoryFactStatusV1 {
        &self.status
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactProjectionV1 {
    Available(Box<ProjectMemoryFactV1>),
    Unavailable(ProjectMemoryFactUnavailableV1),
}

impl ProjectMemoryFactProjectionV1 {
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

    pub fn mapping(&self) -> Option<&ProjectMemoryFactMappingV1> {
        match self {
            Self::Available(fact) => Some(fact.mapping()),
            Self::Unavailable(_) => None,
        }
    }
}
