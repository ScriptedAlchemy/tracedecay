use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactCategoryV1, FactEventId, FactOwnerV1, ProvenanceId,
};

use super::super::super::{
    FactFeedbackAction, FactLineageError, FactLineageResult, MAX_FACT_REASON_BYTES,
};
use super::super::{FactProjection, FactTarget, OwnedFactId, validate_text};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAddCommand {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    content: String,
    category: FactCategoryV1,
    source: Option<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    /// Durable automation identity. This is command metadata, deliberately
    /// separate from the fact payload metadata that passes through privacy
    /// sanitization.
    automation_run_id: Option<String>,
    default_trust: Confidence,
    actor: Option<ActorId>,
}

impl FactAddCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        content: String,
        category: FactCategoryV1,
        source: Option<String>,
        tags: Vec<String>,
        entities: Vec<String>,
        metadata: Value,
        default_trust: Confidence,
        actor: Option<ActorId>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if content.trim().is_empty() {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add content",
            }));
        }
        if source
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add source",
            }));
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            owner,
            operation_id,
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            automation_run_id: None,
            default_trust,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.category
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn entities(&self) -> &[String] {
        &self.entities
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn with_automation_run_id(mut self, run_id: String) -> FactLineageResult<Self> {
        validate_text(&run_id, "compatibility fact automation run identity")?;
        self.automation_run_id = Some(run_id);
        Ok(self)
    }
    pub fn automation_run_id(&self) -> Option<&str> {
        self.automation_run_id.as_deref()
    }
    pub fn default_trust(&self) -> Confidence {
        self.default_trust
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactUpdatePatch {
    content: Option<String>,
    category: Option<FactCategoryV1>,
    source: Option<Option<String>>,
    tags: Option<Vec<String>>,
    entities: Option<Vec<String>>,
    metadata: Option<Value>,
    trust: Option<Confidence>,
}

impl FactUpdatePatch {
    pub fn new(
        content: Option<String>,
        category: Option<FactCategoryV1>,
        source: Option<Option<String>>,
        tags: Option<Vec<String>>,
        entities: Option<Vec<String>>,
        metadata: Option<Value>,
        trust: Option<Confidence>,
    ) -> FactLineageResult<Self> {
        if content.is_none()
            && category.is_none()
            && source.is_none()
            && tags.is_none()
            && entities.is_none()
            && metadata.is_none()
            && trust.is_none()
        {
            return Err(FactLineageError::Contract(DomainError::Empty {
                field: "compatibility fact update patch",
            }));
        }
        if content
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update content",
            }));
        }
        if source.as_ref().is_some_and(|value| {
            value.as_ref().is_some_and(|source| {
                source.trim().is_empty() || source.len() > MAX_FACT_REASON_BYTES
            })
        }) {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update source",
            }));
        }
        Ok(Self {
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            trust,
        })
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn source(&self) -> Option<Option<&str>> {
        self.source.as_ref().map(|value| value.as_deref())
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.entities.as_deref()
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }
    pub fn trust(&self) -> Option<Confidence> {
        self.trust
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactUpdateCommand {
    target: FactTarget,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    patch: FactUpdatePatch,
    actor: Option<ActorId>,
}

impl FactUpdateCommand {
    pub fn new(
        target: FactTarget,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        patch: FactUpdatePatch,
        actor: Option<ActorId>,
    ) -> FactLineageResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            patch,
            actor,
        })
    }

    pub fn target(&self) -> &FactTarget {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn patch(&self) -> &FactUpdatePatch {
        &self.patch
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactRemoveCommand {
    target: FactTarget,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
}

impl FactRemoveCommand {
    pub fn new(
        target: FactTarget,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        actor: Option<ActorId>,
    ) -> FactLineageResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            actor,
        })
    }

    pub fn target(&self) -> &FactTarget {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactFeedbackCommand {
    target: FactTarget,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    action: FactFeedbackAction,
    actor: Option<ActorId>,
    source: Option<String>,
    reason: Option<String>,
}

impl FactFeedbackCommand {
    pub fn new(
        target: FactTarget,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        action: FactFeedbackAction,
        actor: Option<ActorId>,
        source: Option<String>,
        reason: Option<String>,
    ) -> FactLineageResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if source
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback source",
            }));
        }
        if reason
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback reason",
            }));
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            action,
            actor,
            source,
            reason,
        })
    }

    pub fn target(&self) -> &FactTarget {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn action(&self) -> FactFeedbackAction {
        self.action
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactAddDisposition {
    Added,
    NearDuplicate,
    PossibleConflict,
    RejectedSecretLike,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAddOutcome {
    fact: Option<FactProjection>,
    disposition: FactAddDisposition,
    closest_fact_id: Option<OwnedFactId>,
    similarity_millionths: Option<u32>,
    reason: Option<String>,
}

impl FactAddOutcome {
    pub fn new(
        fact: Option<FactProjection>,
        disposition: FactAddDisposition,
        closest_fact_id: Option<OwnedFactId>,
        similarity_millionths: Option<u32>,
        reason: Option<String>,
    ) -> FactLineageResult<Self> {
        if similarity_millionths.is_some_and(|value| value > 1_000_000)
            || reason
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add outcome",
            }));
        }
        Ok(Self {
            fact,
            disposition,
            closest_fact_id,
            similarity_millionths,
            reason,
        })
    }

    pub fn fact(&self) -> Option<&FactProjection> {
        self.fact.as_ref()
    }
    pub fn disposition(&self) -> FactAddDisposition {
        self.disposition
    }
    pub fn closest_fact_id(&self) -> Option<&OwnedFactId> {
        self.closest_fact_id.as_ref()
    }
    pub fn similarity_millionths(&self) -> Option<u32> {
        self.similarity_millionths
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactUpdateOutcome {
    fact: FactProjection,
    trust_delta_millionths: i32,
}

impl FactUpdateOutcome {
    pub fn new(fact: FactProjection, trust_delta_millionths: i32) -> FactLineageResult<Self> {
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update trust delta",
            }));
        }
        Ok(Self {
            fact,
            trust_delta_millionths,
        })
    }

    pub fn fact(&self) -> &FactProjection {
        &self.fact
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactRemoveOutcome {
    /// `None` only for the idempotent no-op disposition: the target never
    /// resolved to a stored fact inside this transaction (never added, or
    /// concurrently removed just before this attempt), so there is no
    /// projection to report. `removed` and `remaining_fact_count` remain
    /// meaningful in that case.
    fact: Option<FactProjection>,
    removed: bool,
    remaining_fact_count: u64,
}

impl FactRemoveOutcome {
    pub fn new(fact: FactProjection, removed: bool, remaining_fact_count: u64) -> Self {
        Self {
            fact: Some(fact),
            removed,
            remaining_fact_count,
        }
    }

    /// Idempotent no-op outcome for a remove target that never resolved to a
    /// stored fact within the authority's single remove transaction.
    /// `removed()` is always `false` here, matching the pre-existing
    /// idempotent-success contract for removing an already-absent fact.
    pub fn not_found(remaining_fact_count: u64) -> Self {
        Self {
            fact: None,
            removed: false,
            remaining_fact_count,
        }
    }

    pub fn fact(&self) -> Option<&FactProjection> {
        self.fact.as_ref()
    }
    pub fn removed(&self) -> bool {
        self.removed
    }
    pub fn remaining_fact_count(&self) -> u64 {
        self.remaining_fact_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactFeedbackOutcome {
    fact: FactProjection,
    event_id: FactEventId,
    /// Numeric event identity from the authoritative V1 mirror.  It is only
    /// present when the adapter durably recorded that mirror row; callers must
    /// not derive it from the canonical event identifier.
    legacy_feedback_event_id: Option<i64>,
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
    helpful_count: u64,
    unhelpful_count: u64,
}

impl FactFeedbackOutcome {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact: FactProjection,
        event_id: FactEventId,
        legacy_feedback_event_id: Option<i64>,
        old_trust: Confidence,
        new_trust: Confidence,
        trust_delta_millionths: i32,
        helpful_count: u64,
        unhelpful_count: u64,
    ) -> FactLineageResult<Self> {
        event_id.validate()?;
        if legacy_feedback_event_id.is_some_and(|value| value <= 0) {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility legacy feedback event id",
            }));
        }
        validate_feedback_trust_delta(old_trust, new_trust, trust_delta_millionths)?;
        Ok(Self {
            fact,
            event_id,
            legacy_feedback_event_id,
            old_trust,
            new_trust,
            trust_delta_millionths,
            helpful_count,
            unhelpful_count,
        })
    }

    pub fn fact(&self) -> &FactProjection {
        &self.fact
    }
    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
    pub fn legacy_feedback_event_id(&self) -> Option<i64> {
        self.legacy_feedback_event_id
    }
    pub fn old_trust(&self) -> Confidence {
        self.old_trust
    }
    pub fn new_trust(&self) -> Confidence {
        self.new_trust
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
}

fn validate_feedback_trust_delta(
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
) -> FactLineageResult<()> {
    let expected = ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths)
        || trust_delta_millionths != expected
    {
        return Err(FactLineageError::Contract(DomainError::NonCanonical {
            field: "compatibility fact feedback trust delta",
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_trust_delta_must_match_the_bound_transition() {
        let old = Confidence::new(0.5).unwrap();
        let new = Confidence::new(0.6).unwrap();
        assert!(validate_feedback_trust_delta(old, new, 100_000).is_ok());
        assert!(validate_feedback_trust_delta(old, new, -100_000).is_err());
    }
}
