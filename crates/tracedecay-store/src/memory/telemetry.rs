use tracedecay_domain::{
    Confidence, DomainError, FactEventId, FactId, FactOwnerV1, PayloadAccessState, UtcMicros,
};

use super::queries::MAX_LINEAGE_LIMIT;
use super::{
    FactLineageCursor, FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_REASON_BYTES,
    MAX_PROJECT_MEMORY_SEARCH_BYTES, validate_owned_fact_id,
};

/// Counters and timestamps project-memory clients expose. They are non-negative by type
/// and stay separate from the immutable fact payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactTelemetryV1 {
    retrieval_count: u64,
    access_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    created_at: UtcMicros,
    updated_at: UtcMicros,
    last_retrieved_at: Option<UtcMicros>,
    last_recalled_at: Option<UtcMicros>,
    last_feedback_at: Option<UtcMicros>,
}

impl ProjectMemoryFactTelemetryV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        retrieval_count: u64,
        access_count: u64,
        helpful_count: u64,
        unhelpful_count: u64,
        created_at: UtcMicros,
        updated_at: UtcMicros,
        last_retrieved_at: Option<UtcMicros>,
        last_recalled_at: Option<UtcMicros>,
        last_feedback_at: Option<UtcMicros>,
    ) -> FactStoreResult<Self> {
        if updated_at < created_at
            || last_retrieved_at.is_some_and(|value| value < created_at)
            || last_recalled_at.is_some_and(|value| value < created_at)
            || last_feedback_at.is_some_and(|value| value < created_at)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "fact telemetry timestamps",
            }));
        }
        Ok(Self {
            retrieval_count,
            access_count,
            helpful_count,
            unhelpful_count,
            created_at,
            updated_at,
            last_retrieved_at,
            last_recalled_at,
            last_feedback_at,
        })
    }

    pub fn retrieval_count(&self) -> u64 {
        self.retrieval_count
    }
    pub fn access_count(&self) -> u64 {
        self.access_count
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
    pub fn created_at(&self) -> UtcMicros {
        self.created_at
    }
    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }
    pub fn last_retrieved_at(&self) -> Option<UtcMicros> {
        self.last_retrieved_at
    }
    pub fn last_recalled_at(&self) -> Option<UtcMicros> {
        self.last_recalled_at
    }
    pub fn last_feedback_at(&self) -> Option<UtcMicros> {
        self.last_feedback_at
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactStatusV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
    payload_access: PayloadAccessState,
    projected_as_of: UtcMicros,
}

impl ProjectMemoryFactStatusV1 {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        payload_access: PayloadAccessState,
        projected_as_of: UtcMicros,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self {
            owner,
            fact_id,
            payload_access,
            projected_as_of,
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
    pub fn payload_access(&self) -> PayloadAccessState {
        self.payload_access
    }
    pub fn projected_as_of(&self) -> UtcMicros {
        self.projected_as_of
    }
}

/// Owner aggregate for the project-memory status response. Counts originate
/// from one authority snapshot rather than handler-side joins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryMemoryFeedbackFunnelV1 {
    retrieval_count_total: u64,
    access_count_total: u64,
    retrieved_fact_count: u64,
    rated_fact_count: u64,
    feedback_total: u64,
    seen_to_feedback_ratio: Option<u64>,
}

impl ProjectMemoryMemoryFeedbackFunnelV1 {
    pub fn new(
        retrieval_count_total: u64,
        access_count_total: u64,
        retrieved_fact_count: u64,
        rated_fact_count: u64,
        feedback_total: u64,
    ) -> Self {
        Self {
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count,
            rated_fact_count,
            feedback_total,
            seen_to_feedback_ratio: (feedback_total != 0)
                .then(|| (retrieval_count_total + access_count_total) / feedback_total),
        }
    }

    pub fn retrieval_count_total(&self) -> u64 {
        self.retrieval_count_total
    }
    pub fn access_count_total(&self) -> u64 {
        self.access_count_total
    }
    pub fn retrieved_fact_count(&self) -> u64 {
        self.retrieved_fact_count
    }
    pub fn rated_fact_count(&self) -> u64 {
        self.rated_fact_count
    }
    pub fn feedback_total(&self) -> u64 {
        self.feedback_total
    }
    pub fn seen_to_feedback_ratio(&self) -> Option<u64> {
        self.seen_to_feedback_ratio
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryMemoryStatusV1 {
    owner: FactOwnerV1,
    fact_count: u64,
    entity_count: u64,
    algebra: ProjectMemoryMemoryAlgebraV1,
    trust_0_025_count: u64,
    trust_025_050_count: u64,
    trust_050_075_count: u64,
    trust_075_100_count: u64,
    below_default_recall_threshold_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    feedback_funnel: ProjectMemoryMemoryFeedbackFunnelV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryMemoryAlgebraV1 {
    name: String,
    hrr_dim: u64,
    estimated_capacity: u64,
}

impl ProjectMemoryMemoryAlgebraV1 {
    pub fn new(name: String, hrr_dim: u64, estimated_capacity: u64) -> FactStoreResult<Self> {
        if name.trim().is_empty() || name.len() > MAX_PROJECT_MEMORY_SEARCH_BYTES {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "memory algebra name",
            }));
        }
        Ok(Self {
            name,
            hrr_dim,
            estimated_capacity,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn hrr_dim(&self) -> u64 {
        self.hrr_dim
    }
    pub fn estimated_capacity(&self) -> u64 {
        self.estimated_capacity
    }
}

impl ProjectMemoryMemoryStatusV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        fact_count: u64,
        entity_count: u64,
        algebra: ProjectMemoryMemoryAlgebraV1,
        trust_0_025_count: u64,
        trust_025_050_count: u64,
        trust_050_075_count: u64,
        trust_075_100_count: u64,
        below_default_recall_threshold_count: u64,
        helpful_count: u64,
        unhelpful_count: u64,
        feedback_funnel: ProjectMemoryMemoryFeedbackFunnelV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        Ok(Self {
            owner,
            fact_count,
            entity_count,
            algebra,
            trust_0_025_count,
            trust_025_050_count,
            trust_050_075_count,
            trust_075_100_count,
            below_default_recall_threshold_count,
            helpful_count,
            unhelpful_count,
            feedback_funnel,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn fact_count(&self) -> u64 {
        self.fact_count
    }
    pub fn entity_count(&self) -> u64 {
        self.entity_count
    }
    pub fn algebra(&self) -> &ProjectMemoryMemoryAlgebraV1 {
        &self.algebra
    }
    pub fn trust_0_025_count(&self) -> u64 {
        self.trust_0_025_count
    }
    pub fn trust_025_050_count(&self) -> u64 {
        self.trust_025_050_count
    }
    pub fn trust_050_075_count(&self) -> u64 {
        self.trust_050_075_count
    }
    pub fn trust_075_100_count(&self) -> u64 {
        self.trust_075_100_count
    }
    pub fn below_default_recall_threshold_count(&self) -> u64 {
        self.below_default_recall_threshold_count
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
    pub fn feedback_funnel(&self) -> &ProjectMemoryMemoryFeedbackFunnelV1 {
        &self.feedback_funnel
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactFeedbackActionV1 {
    Helpful,
    Unhelpful,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactFeedbackDetailsAvailabilityV1 {
    Available,
    Redacted,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactFeedbackHistoryEntryV1 {
    event_id: FactEventId,
    occurred_at: UtcMicros,
    action: ProjectMemoryFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    source: Option<String>,
    note: Option<String>,
    details_availability: ProjectMemoryFactFeedbackDetailsAvailabilityV1,
}

impl ProjectMemoryFactFeedbackHistoryEntryV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: FactEventId,
        occurred_at: UtcMicros,
        action: ProjectMemoryFactFeedbackActionV1,
        old_trust: Confidence,
        new_trust: Confidence,
        source: Option<String>,
        note: Option<String>,
        details_availability: ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ) -> FactStoreResult<Self> {
        event_id.validate()?;
        if source.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) || note.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "fact feedback history details",
            }));
        }
        let has_details = source.is_some() || note.is_some();
        if (details_availability == ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available)
            != has_details
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "fact feedback details availability",
            }));
        }
        Ok(Self {
            event_id,
            occurred_at,
            action,
            old_trust,
            new_trust,
            source,
            note,
            details_availability,
        })
    }

    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
    pub fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }
    pub fn action(&self) -> ProjectMemoryFactFeedbackActionV1 {
        self.action
    }
    pub fn old_trust(&self) -> Confidence {
        self.old_trust
    }
    pub fn new_trust(&self) -> Confidence {
        self.new_trust
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    pub fn details_availability(&self) -> ProjectMemoryFactFeedbackDetailsAvailabilityV1 {
        self.details_availability
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactFeedbackHistoryV1 {
    owner: FactOwnerV1,
    events: Vec<ProjectMemoryFactFeedbackHistoryEntryV1>,
    next_after: Option<FactLineageCursor>,
}

impl ProjectMemoryFactFeedbackHistoryV1 {
    pub fn new(
        owner: FactOwnerV1,
        events: Vec<ProjectMemoryFactFeedbackHistoryEntryV1>,
        next_after: Option<FactLineageCursor>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if events.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: events.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&ProjectMemoryFactFeedbackHistoryEntryV1> = None;
        for event in &events {
            if previous.is_some_and(|value| {
                (value.occurred_at(), value.event_id()) >= (event.occurred_at(), event.event_id())
            }) {
                return Err(FactStoreError::EventsOutOfOrder);
            }
            previous = Some(event);
        }
        Ok(Self {
            owner,
            events,
            next_after,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn events(&self) -> &[ProjectMemoryFactFeedbackHistoryEntryV1] {
        &self.events
    }
    pub fn next_after(&self) -> Option<&FactLineageCursor> {
        self.next_after.as_ref()
    }
}
