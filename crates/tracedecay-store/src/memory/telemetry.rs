use tracedecay_domain::{
    Confidence, DomainError, FactEventId, FactId, FactOwnerV1, PayloadAccessState, UtcMicros,
    VectorWatermark,
};

use super::queries::MAX_LINEAGE_LIMIT;
use super::{
    FactLineageCursor, FactStoreError, FactStoreResult, MAX_COMPATIBILITY_REASON_BYTES,
    MAX_COMPATIBILITY_SEARCH_BYTES, validate_owned_fact_id,
};

/// Counters and timestamps V1 clients expose.  They are non-negative by type
/// and stay separate from the immutable fact payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactTelemetryV1 {
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

impl CompatibilityFactTelemetryV1 {
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
                field: "compatibility fact telemetry timestamps",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityProjectionStateV1 {
    Ready,
    Rebuilding,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactStatusV1 {
    owner: FactOwnerV1,
    fact_id: Option<FactId>,
    payload_access: Option<PayloadAccessState>,
    projection_state: CompatibilityProjectionStateV1,
    projected_as_of: Option<UtcMicros>,
    vector_watermark: Option<VectorWatermark>,
}

impl CompatibilityFactStatusV1 {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: Option<FactId>,
        payload_access: Option<PayloadAccessState>,
        projection_state: CompatibilityProjectionStateV1,
        projected_as_of: Option<UtcMicros>,
        vector_watermark: Option<VectorWatermark>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        Ok(Self {
            owner,
            fact_id,
            payload_access,
            projection_state,
            projected_as_of,
            vector_watermark,
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
    pub fn fact_id(&self) -> Option<&FactId> {
        self.fact_id.as_ref()
    }
    pub fn payload_access(&self) -> Option<PayloadAccessState> {
        self.payload_access
    }
    pub fn projection_state(&self) -> CompatibilityProjectionStateV1 {
        self.projection_state
    }
    pub fn projected_as_of(&self) -> Option<UtcMicros> {
        self.projected_as_of
    }
    pub fn vector_watermark(&self) -> Option<&VectorWatermark> {
        self.vector_watermark.as_ref()
    }
}

/// Owner aggregate for the legacy memory-status response.  Counts originate
/// from one authority snapshot rather than handler-side joins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryFeedbackFunnelV1 {
    retrieval_count_total: u64,
    access_count_total: u64,
    retrieved_fact_count: u64,
    rated_fact_count: u64,
    feedback_total: u64,
    seen_to_feedback_ratio: Option<u64>,
}

impl CompatibilityMemoryFeedbackFunnelV1 {
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
pub struct CompatibilityMemoryStatusV1 {
    owner: FactOwnerV1,
    fact_count: u64,
    entity_count: u64,
    bank_count: u64,
    algebra: CompatibilityMemoryAlgebraV1,
    trust_0_025_count: u64,
    trust_025_050_count: u64,
    trust_050_075_count: u64,
    trust_075_100_count: u64,
    below_default_recall_threshold_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    missing_vector_count: u64,
    projection_state: CompatibilityProjectionStateV1,
    repair: CompatibilityMemoryRepairStatsV1,
    feedback_history_repair: CompatibilityFeedbackRepairProgressV1,
    feedback_funnel: CompatibilityMemoryFeedbackFunnelV1,
}

/// Bounded migration/repair state for V1 feedback history. A request may report
/// incomplete work, but never hides it by returning an empty or fabricated
/// history while the daemon continues the remaining batches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompatibilityFeedbackRepairProgressV1 {
    /// No V2 history projection exists for this owner yet.
    #[default]
    Unknown,
    /// No repair is needed for this owner.
    NotRequired,
    /// Repair is complete. `processed` is the work done by the observed run.
    Complete { processed: u64 },
    /// One bounded repair call advanced `processed` items; remaining count may
    /// be deliberately unknown without a costly full scan.
    Incomplete {
        processed: u64,
        remaining: Option<u64>,
    },
}

impl CompatibilityFeedbackRepairProgressV1 {
    pub fn is_complete(self) -> bool {
        matches!(self, Self::NotRequired | Self::Complete { .. })
    }

    pub fn processed(self) -> u64 {
        match self {
            Self::Unknown | Self::NotRequired => 0,
            Self::Complete { processed } | Self::Incomplete { processed, .. } => processed,
        }
    }

    pub fn remaining(self) -> Option<u64> {
        match self {
            Self::Incomplete { remaining, .. } => remaining,
            Self::Unknown => None,
            Self::NotRequired | Self::Complete { .. } => Some(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompatibilityMemoryRepairStatsV1 {
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
    /// Exact feedback-history batch outcome when this is an explicit repair
    /// receipt. Other repair-producing paths leave this `Unknown`.
    feedback_history_repair: CompatibilityFeedbackRepairProgressV1,
    /// Whether the producing repair pass filled a per-pass batch cap and may
    /// have more backlog behind it. Computed by the store, which alone knows
    /// the caps; consumers (e.g. the daemon scheduler) read [`Self::saturated`]
    /// instead of comparing counters against store-internal batch constants.
    saturated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryAlgebraV1 {
    name: String,
    hrr_dim: u64,
    estimated_capacity: u64,
}

impl CompatibilityMemoryAlgebraV1 {
    pub fn new(name: String, hrr_dim: u64, estimated_capacity: u64) -> FactStoreResult<Self> {
        if name.trim().is_empty() || name.len() > MAX_COMPATIBILITY_SEARCH_BYTES {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility memory algebra name",
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

impl CompatibilityMemoryRepairStatsV1 {
    pub fn new(missing_vectors_repaired: u64, banks_rebuilt: u64) -> Self {
        Self {
            missing_vectors_repaired,
            banks_rebuilt,
            feedback_history_repair: CompatibilityFeedbackRepairProgressV1::Unknown,
            saturated: false,
        }
    }

    pub fn with_feedback_history_repair(
        mut self,
        feedback_history_repair: CompatibilityFeedbackRepairProgressV1,
    ) -> Self {
        self.feedback_history_repair = feedback_history_repair;
        self
    }

    /// Records whether the producing repair pass filled a per-pass batch cap.
    /// Only the store computes this, since it alone knows the batch caps.
    pub fn with_saturated(mut self, saturated: bool) -> Self {
        self.saturated = saturated;
        self
    }

    pub fn missing_vectors_repaired(&self) -> u64 {
        self.missing_vectors_repaired
    }
    pub fn banks_rebuilt(&self) -> u64 {
        self.banks_rebuilt
    }
    pub fn feedback_history_repair(&self) -> CompatibilityFeedbackRepairProgressV1 {
        self.feedback_history_repair
    }
    /// True when the producing repair pass filled a per-pass batch cap and may
    /// have more backlog behind it. Lets the daemon scheduler keep ticking
    /// without depending on store-internal batch constants.
    pub fn saturated(&self) -> bool {
        self.saturated
    }
}

impl CompatibilityMemoryStatusV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        fact_count: u64,
        entity_count: u64,
        bank_count: u64,
        algebra: CompatibilityMemoryAlgebraV1,
        trust_0_025_count: u64,
        trust_025_050_count: u64,
        trust_050_075_count: u64,
        trust_075_100_count: u64,
        below_default_recall_threshold_count: u64,
        helpful_count: u64,
        unhelpful_count: u64,
        missing_vector_count: u64,
        projection_state: CompatibilityProjectionStateV1,
        repair: CompatibilityMemoryRepairStatsV1,
        feedback_funnel: CompatibilityMemoryFeedbackFunnelV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        Ok(Self {
            owner,
            fact_count,
            entity_count,
            bank_count,
            algebra,
            trust_0_025_count,
            trust_025_050_count,
            trust_050_075_count,
            trust_075_100_count,
            below_default_recall_threshold_count,
            helpful_count,
            unhelpful_count,
            missing_vector_count,
            projection_state,
            repair,
            feedback_history_repair: CompatibilityFeedbackRepairProgressV1::Unknown,
            feedback_funnel,
        })
    }

    pub fn with_feedback_history_repair(
        mut self,
        feedback_history_repair: CompatibilityFeedbackRepairProgressV1,
    ) -> Self {
        self.feedback_history_repair = feedback_history_repair;
        self
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
    pub fn bank_count(&self) -> u64 {
        self.bank_count
    }
    pub fn algebra(&self) -> &CompatibilityMemoryAlgebraV1 {
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
    pub fn missing_vector_count(&self) -> u64 {
        self.missing_vector_count
    }
    pub fn feedback_history_repair(&self) -> CompatibilityFeedbackRepairProgressV1 {
        self.feedback_history_repair
    }
    pub fn projection_state(&self) -> CompatibilityProjectionStateV1 {
        self.projection_state
    }
    pub fn repair(&self) -> CompatibilityMemoryRepairStatsV1 {
        self.repair
    }
    pub fn feedback_funnel(&self) -> &CompatibilityMemoryFeedbackFunnelV1 {
        &self.feedback_funnel
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactFeedbackActionV1 {
    Helpful,
    Unhelpful,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactFeedbackDetailsAvailabilityV1 {
    Available,
    LegacyRedacted,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackHistoryEntryV1 {
    event_id: FactEventId,
    occurred_at: UtcMicros,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    source: Option<String>,
    note: Option<String>,
    details_availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
}

impl CompatibilityFactFeedbackHistoryEntryV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: FactEventId,
        occurred_at: UtcMicros,
        action: CompatibilityFactFeedbackActionV1,
        old_trust: Confidence,
        new_trust: Confidence,
        source: Option<String>,
        note: Option<String>,
        details_availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
    ) -> FactStoreResult<Self> {
        event_id.validate()?;
        if source.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) || note.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback history details",
            }));
        }
        if details_availability != CompatibilityFactFeedbackDetailsAvailabilityV1::Available
            && (source.is_some() || note.is_some())
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback redacted details",
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
    pub fn action(&self) -> CompatibilityFactFeedbackActionV1 {
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
    pub fn details_availability(&self) -> CompatibilityFactFeedbackDetailsAvailabilityV1 {
        self.details_availability
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackHistoryV1 {
    owner: FactOwnerV1,
    events: Vec<CompatibilityFactFeedbackHistoryEntryV1>,
    next_after: Option<FactLineageCursor>,
    repair_progress: CompatibilityFeedbackRepairProgressV1,
}

impl CompatibilityFactFeedbackHistoryV1 {
    pub fn new(
        owner: FactOwnerV1,
        events: Vec<CompatibilityFactFeedbackHistoryEntryV1>,
        next_after: Option<FactLineageCursor>,
    ) -> FactStoreResult<Self> {
        Self::new_with_repair_progress(
            owner,
            events,
            next_after,
            CompatibilityFeedbackRepairProgressV1::Unknown,
        )
    }

    pub fn new_with_repair_progress(
        owner: FactOwnerV1,
        events: Vec<CompatibilityFactFeedbackHistoryEntryV1>,
        next_after: Option<FactLineageCursor>,
        repair_progress: CompatibilityFeedbackRepairProgressV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if events.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: events.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&CompatibilityFactFeedbackHistoryEntryV1> = None;
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
            repair_progress,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn events(&self) -> &[CompatibilityFactFeedbackHistoryEntryV1] {
        &self.events
    }
    pub fn next_after(&self) -> Option<&FactLineageCursor> {
        self.next_after.as_ref()
    }
    pub fn repair_progress(&self) -> CompatibilityFeedbackRepairProgressV1 {
        self.repair_progress
    }
}
