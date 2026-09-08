//! Revisioned Work blocked-interval receipts offered to the shared producer.

use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, WorkBlockedIntervalReceiptV1,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

/// Offers one exact open or settled interval receipt without awaiting telemetry.
///
/// Open revision one makes an active blocker visible at the watermark. Settled
/// revision two closes that same trace with the source-owned end instant. A
/// producer enqueue is not a durable outbox acknowledgement, so recovery marks
/// only a settled receipt after its exact revision-two fact is durably claimed.
pub fn record_work_blocked_interval_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    canonical_project_scope: &str,
    receipt: &WorkBlockedIntervalReceiptV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let envelope = match work_blocked_interval_observation_envelope(
        producer,
        canonical_project_scope,
        receipt,
    ) {
        Ok(envelope) => envelope,
        Err(_) => return WorkOwnerObservationResultV1::Unavailable,
    };
    match producer.try_emit_owner_fact(envelope) {
        Ok(ObservabilityEmissionOutcomeV1::Enqueued) => WorkOwnerObservationResultV1::Enqueued,
        Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
            WorkOwnerObservationResultV1::DroppedAtCapacity
        }
        Err(_) => WorkOwnerObservationResultV1::Unavailable,
    }
}

/// Builds the canonical owner envelope for one blocked-interval revision.
///
/// Background recovery uses this same builder for settled receipts, so the
/// response-path offer and retained delivery cannot drift in event identity,
/// valid time, or payload semantics.
pub fn work_blocked_interval_observation_envelope(
    producer: &BoundedObservabilityProducerV1,
    canonical_project_scope: &str,
    receipt: &WorkBlockedIntervalReceiptV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let (event_time, valid_until) = match (
        receipt.interval_revision(),
        receipt.ended_at(),
        receipt.closure(),
    ) {
        (1, None, None) => (receipt.started_at(), None),
        (2, Some(ended_at), Some(_)) => (ended_at, Some(ended_at)),
        _ => return Err("work_blocked_interval_revision"),
    };
    let payload = receipt.observability_payload();
    if payload.validate().is_err()
        || payload.valid_from_micros != receipt.started_at().0
        || payload.valid_until_micros != valid_until.map(|ended_at| ended_at.0)
        || payload.coverage != CoverageStateV1::Known
    {
        return Err("work_blocked_interval_payload");
    }
    let owner_transition_ref = match receipt.observation_ref() {
        Ok(reference) => reference,
        Err(_) => return Err("work_blocked_interval_identity"),
    };
    execution_owner_fact_envelope(
        producer.identity(),
        canonical_project_scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: "work_blocked_interval",
            event_time,
            valid_from: Some(receipt.started_at()),
            valid_until,
            terminal_result: None,
            coverage: CoverageStateV1::Known,
            payload: ObservabilityPayloadV1::WorkBlockedInterval(payload),
        },
    )
}
