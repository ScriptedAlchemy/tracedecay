//! Durable Plan 26 observations projected from Work retry/leak receipts.

use tracedecay_application::{WorkLeakAdjudicationReceiptV1, WorkRetryReceiptV1};
use tracedecay_domain::{
    CoverageStateV1, DurationBucketV1, ObservabilityPayloadV1, RerunCauseV1, RerunSourceV1,
    WorkRerunObservedV1,
};

use super::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, execution_owner_fact_envelope,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkOwnerObservationResultV1 {
    Enqueued,
    DroppedAtCapacity,
    Unavailable,
}

/// Offers the exact new-attempt retry receipt to the durable observability
/// outbox. No attempt count, cause, time, or lineage is inferred from rows.
pub fn record_work_retry_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    canonical_project_scope: &str,
    receipt: &WorkRetryReceiptV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Some(envelope) =
        work_retry_observation_envelope(producer.identity(), canonical_project_scope, receipt)
    else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    offer(producer, envelope)
}

pub(crate) fn work_retry_observation_envelope(
    identity: &ObservabilityProducerIdentityV1,
    canonical_project_scope: &str,
    receipt: &WorkRetryReceiptV1,
) -> Option<tracedecay_domain::ObservabilityEnvelopeV1> {
    if !receipt.validate_for_observation() {
        return None;
    }
    let latency = receipt
        .restarted_at
        .0
        .saturating_sub(receipt.retry_required_at.0);
    let Ok(latency) = u64::try_from(latency) else {
        return None;
    };
    let payload = ObservabilityPayloadV1::WorkRerun(WorkRerunObservedV1 {
        source: RerunSourceV1::Runtime,
        cause: RerunCauseV1::RuntimeRetry,
        eligible_original_count: 1,
        linked_rerun_count: 1,
        latency_bucket: duration_bucket(latency),
        coverage: CoverageStateV1::Known,
    });
    let owner_ref = format!(
        "work-retry:{}:{}",
        receipt.command.command_id.as_str(),
        receipt.new_attempt.attempt_id().as_str()
    );
    execution_owner_fact_envelope(
        identity,
        canonical_project_scope,
        &owner_ref,
        "retry_work_attempt",
        receipt.restarted_at,
        Some(receipt.retry_required_at),
        Some(receipt.restarted_at),
        None,
        CoverageStateV1::Known,
        payload,
    )
    .ok()
}

/// Offers the exact bounded-scan leak verdict to the durable observability
/// outbox. Corrections use a distinct receipt revision and retain prior facts.
pub fn record_work_leak_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    canonical_project_scope: &str,
    receipt: &WorkLeakAdjudicationReceiptV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Some(envelope) =
        work_leak_observation_envelope(producer.identity(), canonical_project_scope, receipt)
    else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    offer(producer, envelope)
}

pub(crate) fn work_leak_observation_envelope(
    identity: &ObservabilityProducerIdentityV1,
    canonical_project_scope: &str,
    receipt: &WorkLeakAdjudicationReceiptV1,
) -> Option<tracedecay_domain::ObservabilityEnvelopeV1> {
    if !receipt.validate_for_observation() {
        return None;
    }
    let owner_ref = match receipt.adjudication_ref() {
        Ok(reference) => format!("work-leak:{}", reference.as_str()),
        Err(_) => return None,
    };
    execution_owner_fact_envelope(
        identity,
        canonical_project_scope,
        &owner_ref,
        "adjudicate_work_leak",
        receipt.evidence.scan_completed_at,
        Some(receipt.evidence.scan_started_at),
        Some(receipt.evidence.scan_completed_at),
        None,
        receipt.evidence.coverage,
        match receipt.observability_payload() {
            Ok(payload) => ObservabilityPayloadV1::WorkExecutionLeak(payload),
            Err(_) => return None,
        },
    )
    .ok()
}

fn offer(
    producer: &BoundedObservabilityProducerV1,
    envelope: tracedecay_domain::ObservabilityEnvelopeV1,
) -> WorkOwnerObservationResultV1 {
    match producer.try_emit_owner_fact(envelope) {
        Ok(ObservabilityEmissionOutcomeV1::Enqueued) => WorkOwnerObservationResultV1::Enqueued,
        Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
            WorkOwnerObservationResultV1::DroppedAtCapacity
        }
        Err(_) => WorkOwnerObservationResultV1::Unavailable,
    }
}

const fn duration_bucket(micros: u64) -> DurationBucketV1 {
    const MINUTE: u64 = 60_000_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match micros {
        value if value < MINUTE => DurationBucketV1::Under1m,
        value if value < 5 * MINUTE => DurationBucketV1::From1mTo5m,
        value if value < 15 * MINUTE => DurationBucketV1::From5mTo15m,
        value if value < HOUR => DurationBucketV1::From15mTo1h,
        value if value < 4 * HOUR => DurationBucketV1::From1hTo4h,
        value if value < DAY => DurationBucketV1::From4hTo24h,
        value if value < 7 * DAY => DurationBucketV1::From1dTo7d,
        _ => DurationBucketV1::Over7d,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::{
        AdjudicateWorkLeakCommandV1, RetryWorkAttemptCommandV1, VerifiedWorkLeakEvidenceV1,
        VerifiedWorkRetryFailureV1,
    };
    use tracedecay_domain::{
        AttemptId, LeakOwnerClassV1, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
        WorkCommandId, WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1, canonical_sha256,
    };

    fn identity() -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: "project:retry-leak".to_owned(),
            process_boot_id: "boot:retry-leak".to_owned(),
            producer_revision: "producer.v1".to_owned(),
            configuration_revision: "configuration.v1".to_owned(),
            policy_revision: "policy.v1".to_owned(),
        }
    }

    fn attempt(attempt_id: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            TaskId::new("task.retry-leak".to_owned()).expect("task id"),
            RunId::new("run.retry-leak".to_owned()).expect("run id"),
            AttemptId::new(attempt_id.to_owned()).expect("attempt id"),
        )
        .expect("attempt identity")
    }

    fn retry_receipt(evidence_ref: &str) -> WorkRetryReceiptV1 {
        let command = RetryWorkAttemptCommandV1 {
            original_attempt: attempt("attempt.original"),
            new_attempt_id: AttemptId::new("attempt.rerun".to_owned()).expect("attempt id"),
            failure: tracedecay_application::WorkRetryFailureSelectorV1 {
                source: tracedecay_application::WorkRetrySourceV1::Runtime,
                cause: tracedecay_application::WorkRetryCauseV1::RuntimeFailure,
                evidence_ref: evidence_ref.to_owned(),
            },
            command_id: WorkCommandId::new(format!("command.{evidence_ref}")).expect("command id"),
        };
        let evidence_digest = canonical_sha256(&("source-owned-retry-evidence.v1", evidence_ref))
            .expect("evidence digest");
        let failure = VerifiedWorkRetryFailureV1 {
            selector: command.failure.clone(),
            evidence_digest,
            observed_at: UtcMicros(1_900),
        };
        WorkRetryReceiptV1::new(
            command,
            failure,
            attempt("attempt.rerun"),
            UtcMicros(1_900),
            UtcMicros(2_100),
        )
        .expect("retry receipt")
    }

    #[test]
    fn runtime_retry_receipt_keeps_source_cause_and_replay_identity() {
        let receipt = retry_receipt("runtime-terminal:retry-evidence");
        let first = work_retry_observation_envelope(&identity(), "project:retry-leak", &receipt)
            .expect("verified retry observation");
        let replay = work_retry_observation_envelope(&identity(), "project:retry-leak", &receipt)
            .expect("verified retry replay");
        assert_eq!(first, replay);
        let ObservabilityPayloadV1::WorkRerun(observed) = first.payload else {
            panic!("expected Work rerun observation");
        };
        assert_eq!(observed.source, RerunSourceV1::Runtime);
        assert_eq!(observed.cause, RerunCauseV1::RuntimeRetry);
        assert_eq!(observed.eligible_original_count, 1);
        assert_eq!(observed.linked_rerun_count, 1);
        assert_eq!(observed.coverage, CoverageStateV1::Known);
    }

    fn leak_receipt(
        kind: WorkExecutionLeakKindV1,
        owner_class: LeakOwnerClassV1,
    ) -> WorkLeakAdjudicationReceiptV1 {
        let attempt = attempt("attempt.leak");
        let command = AdjudicateWorkLeakCommandV1 {
            adjudication_id: "adjudication.leak".to_owned(),
            expected_revision: None,
            attempt: attempt.clone(),
            detection_horizon_micros: 1_000,
            command_id: WorkCommandId::new("command.leak".to_owned()).expect("command id"),
        };
        let evidence = VerifiedWorkLeakEvidenceV1 {
            attempt,
            kind,
            recovery: WorkExecutionLeakRecoveryV1::Pending,
            owner_class,
            coverage: CoverageStateV1::Known,
            detection_horizon_micros: command.detection_horizon_micros,
            scan_started_at: UtcMicros(3_100),
            scan_completed_at: UtcMicros(3_200),
            evidence_refs: vec!["source-owned:exact-binding".to_owned()],
        };
        WorkLeakAdjudicationReceiptV1 {
            scan_deadline: UtcMicros(4_000),
            canonical_input_digest: canonical_sha256(&(
                "tracedecay.application.work-leak-adjudication.v1",
                &command,
                &evidence,
                UtcMicros(4_000),
            ))
            .expect("input digest"),
            command,
            revision: 1,
            evidence,
        }
    }

    #[test]
    fn source_bound_effect_worktree_and_delivery_leaks_are_not_collapsed_to_unknown() {
        for (kind, owner_class) in [
            (
                WorkExecutionLeakKindV1::EffectUnknownPastDeadline,
                LeakOwnerClassV1::Work,
            ),
            (
                WorkExecutionLeakKindV1::MissingWorktreeBinding,
                LeakOwnerClassV1::Work,
            ),
            (
                WorkExecutionLeakKindV1::UnboundedDelivery,
                LeakOwnerClassV1::Delivery,
            ),
        ] {
            let receipt = leak_receipt(kind, owner_class);
            let envelope =
                work_leak_observation_envelope(&identity(), "project:retry-leak", &receipt)
                    .expect("verified leak observation");
            let ObservabilityPayloadV1::WorkExecutionLeak(observed) = envelope.payload else {
                panic!("expected Work execution leak observation");
            };
            assert_eq!(observed.kind, kind);
            assert_eq!(observed.owner_class, owner_class);
            assert_eq!(observed.coverage, CoverageStateV1::Known);
            assert_eq!(observed.detection_horizon_micros, 1_000);
        }
    }

    #[test]
    fn emitter_rejects_tampered_source_binding_receipts() {
        let mut retry = retry_receipt("runtime-terminal:retry-evidence");
        retry.failure.evidence_digest =
            canonical_sha256(&("tampered-retry-evidence.v1", 1_u8)).expect("digest");
        assert!(
            work_retry_observation_envelope(&identity(), "project:retry-leak", &retry).is_none()
        );

        let mut leak = leak_receipt(
            WorkExecutionLeakKindV1::UnboundedDelivery,
            LeakOwnerClassV1::Delivery,
        );
        leak.evidence.attempt = attempt("attempt.other");
        assert!(work_leak_observation_envelope(&identity(), "project:retry-leak", &leak).is_none());
    }
}
