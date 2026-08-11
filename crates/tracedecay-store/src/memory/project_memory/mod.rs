use serde_json::Value;
use tracedecay_domain::canonical_text::is_canonical_text_within;
use tracedecay_domain::{
    Confidence, DomainError, FactAssertionId, FactCategoryV1, FactEventId, FactId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizerDispositionV1, UtcMicros,
};

use super::queries::{MAX_CURRENT_LIMIT, MAX_LINEAGE_LIMIT};
use super::{
    FactLineageCursor, FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_SEARCH_BYTES,
    ProjectMemoryFactStatusV1, ProjectMemoryFactTelemetryV1, validate_owned_fact_id,
};

mod automatic_facts;
mod curation;
pub(super) mod dashboard;
mod search;

pub use automatic_facts::{
    MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEffectV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptPageV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
};
pub use curation::{
    ProjectMemoryEntityIdV1, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddMaterialV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackOutcomeV1, ProjectMemoryFactLinkV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactNormalizeTagsV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryFactUpdatePatchV1,
};
pub use dashboard::{
    ProjectMemoryDashboardEntityV1, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardFactEntityLinkV1,
    ProjectMemoryDashboardFactSummaryV1, ProjectMemoryDashboardGrowthPointV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardNamedCountV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1,
};
pub use search::{
    MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactContradictionV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactRetrievalOutcomeV1,
    ProjectMemoryFactRetrievalReceiptV1, ProjectMemoryFactSearchCursorV1,
    ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchGraphCoverageV1,
    ProjectMemoryFactSearchGraphDegradationV1, ProjectMemoryFactSearchHitV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchScoresV1,
};

fn validate_project_memory_entity(value: &str) -> FactStoreResult<()> {
    validate_project_memory_text(value, "fact entity")
}

fn validate_project_memory_text(value: &str, field: &'static str) -> FactStoreResult<()> {
    if !is_canonical_text_within(value, MAX_PROJECT_MEMORY_SEARCH_BYTES) {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field,
        }));
    }
    Ok(())
}

/// Stable owner-bound canonical fact identity.
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

/// Available projection of one canonical fact. Its required payload is the
/// sole payload copy and therefore makes eligibility structural.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactV1 {
    fact_id: FactId,
    owner: FactOwnerV1,
    payload: FactPayloadV1,
    trust: Confidence,
    active_assertion_id: FactAssertionId,
    last_event_id: FactEventId,
    projected_as_of: UtcMicros,
    source: FactIdentitySourceV1,
    telemetry: ProjectMemoryFactTelemetryV1,
}

impl ProjectMemoryFactV1 {
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        payload: FactPayloadV1,
        trust: Confidence,
        active_assertion_id: FactAssertionId,
        last_event_id: FactEventId,
        projected_as_of: UtcMicros,
        source: FactIdentitySourceV1,
        telemetry: ProjectMemoryFactTelemetryV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if payload.receipt().disposition() != SanitizerDispositionV1::Accepted {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        validate_owned_fact_id(&fact_id, &owner)?;
        active_assertion_id.validate()?;
        last_event_id.validate()?;
        let material = FactIdentityMaterialV1::new(owner.clone(), source.clone())?;
        if FactId::derive(&material)? != fact_id {
            return Err(FactStoreError::FactMismatch);
        }
        if telemetry.updated_at() != projected_as_of {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "fact projection snapshot",
            }));
        }
        Ok(Self {
            fact_id,
            owner,
            payload,
            trust,
            active_assertion_id,
            last_event_id,
            projected_as_of,
            source,
            telemetry,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if self.owner() != owner {
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
    pub fn trust(&self) -> Confidence {
        self.trust
    }
    pub fn active_assertion_id(&self) -> &FactAssertionId {
        &self.active_assertion_id
    }
    pub fn last_event_id(&self) -> &FactEventId {
        &self.last_event_id
    }
    pub fn projected_as_of(&self) -> UtcMicros {
        self.projected_as_of
    }
    pub fn source(&self) -> &FactIdentitySourceV1 {
        &self.source
    }
    pub fn source_label(&self) -> Option<&str> {
        self.payload().source_label()
    }
    pub fn telemetry(&self) -> &ProjectMemoryFactTelemetryV1 {
        &self.telemetry
    }
    pub fn payload(&self) -> &FactPayloadV1 {
        &self.payload
    }
    pub fn content(&self) -> &str {
        self.payload().content()
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.payload().category()
    }
    pub fn tags(&self) -> &[String] {
        self.payload().tags()
    }
    pub fn entities(&self) -> &[String] {
        self.payload().entities()
    }
    pub fn metadata(&self) -> &Value {
        self.payload().metadata()
    }
}

/// A bounded, deterministic fact page. Facts are sorted by
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
                    field: "fact page order",
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
                    field: "fact page cursor",
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

/// Bounded detail projection used for canonical `get`, history, status, and dashboard
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
        if history.fact_id() != fact.fact_id() || status.fact_id() != fact.fact_id() {
            return Err(FactStoreError::FactMismatch);
        }
        if status.payload_access() != tracedecay_domain::PayloadAccessState::Eligible {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        if status.projected_as_of() != fact.projected_as_of() {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "fact inspection snapshot",
            }));
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
                    field: "fact inspection anchors",
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

/// Safe representation for a fact whose canonical payload-access state does
/// not permit an available payload projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactUnavailableV1 {
    status: ProjectMemoryFactStatusV1,
}

impl ProjectMemoryFactUnavailableV1 {
    pub fn new(status: ProjectMemoryFactStatusV1) -> FactStoreResult<Self> {
        if status.payload_access() == tracedecay_domain::PayloadAccessState::Eligible {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        Ok(Self { status })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.status.owner()
    }
    pub fn fact_id(&self) -> &FactId {
        self.status.fact_id()
    }
    pub fn payload_access(&self) -> tracedecay_domain::PayloadAccessState {
        self.status.payload_access()
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
            Self::Unavailable(fact) => fact.owner(),
        }
    }

    pub fn fact_id(&self) -> &FactId {
        match self {
            Self::Available(fact) => fact.fact_id(),
            Self::Unavailable(fact) => fact.fact_id(),
        }
    }
}
