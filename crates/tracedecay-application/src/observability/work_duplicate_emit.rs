//! Receipt-derived duplicate-Work observations for the shared bounded producer.

use tracedecay_domain::{
    ObservabilityPayloadV1, ObservabilityTerminalResultV1, WorkAuthority,
    WorkDuplicateAdjudicationReceiptV1,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

/// Offers one exact durable adjudication receipt to the long-lived producer.
///
/// The receipt is reconstructed before projection so a deserialized lookalike
/// cannot become telemetry. Queue pressure is the producer's typed drop state
/// and never changes the already-committed Work result.
pub fn record_work_duplicate_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    canonical_project_scope: &str,
    authority: &WorkAuthority,
    receipt: &WorkDuplicateAdjudicationReceiptV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Some(envelope) = work_duplicate_observation_envelope(
        producer.identity(),
        canonical_project_scope,
        authority,
        receipt,
    ) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    match producer.try_emit_owner_fact(envelope) {
        Ok(ObservabilityEmissionOutcomeV1::Enqueued) => WorkOwnerObservationResultV1::Enqueued,
        Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
            WorkOwnerObservationResultV1::DroppedAtCapacity
        }
        Err(_) => WorkOwnerObservationResultV1::Unavailable,
    }
}

pub(crate) fn work_duplicate_observation_envelope(
    identity: &ObservabilityProducerIdentityV1,
    canonical_project_scope: &str,
    authority: &WorkAuthority,
    receipt: &WorkDuplicateAdjudicationReceiptV1,
) -> Option<tracedecay_domain::ObservabilityEnvelopeV1> {
    if receipt.actor_id() != authority.actor_id() {
        return None;
    }
    let canonical = match WorkDuplicateAdjudicationReceiptV1::new(
        authority,
        receipt.command().clone(),
        receipt.revision(),
        receipt.canonical_input_digest().clone(),
    ) {
        Ok(canonical) if canonical == *receipt => canonical,
        Ok(_) | Err(_) => return None,
    };
    let adjudication_ref = canonical.adjudication_ref();
    let (coverage, payload) = match canonical.observability_payload() {
        payload if payload.validate().is_ok() => (
            payload.coverage,
            ObservabilityPayloadV1::WorkDuplicateEffort(payload),
        ),
        _ => return None,
    };
    let owner_transition_ref = format!("work-duplicate:{}", adjudication_ref.as_str());
    let occurred_at = canonical.command().occurred_at;
    let envelope = match execution_owner_fact_envelope(
        identity,
        canonical_project_scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: "adjudicate_duplicate",
            event_time: occurred_at,
            valid_from: Some(occurred_at),
            valid_until: Some(occurred_at),
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            coverage,
            payload,
        },
    ) {
        Ok(envelope) => envelope,
        Err(_) => return None,
    };
    Some(envelope)
}
