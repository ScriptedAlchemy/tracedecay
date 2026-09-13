//! Exact operation-resource receipts for settled Work provider attempts.

use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityTerminalResultV1, OperationResourceObservedV1, UtcMicros, WorkAttemptIdentityV1,
    WorkAttemptStateV1, WorkAttemptV1,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

/// Offers one resource receipt only for the exact persisted terminal attempt.
///
/// Timing is supplied by the same-process execution owner. Pre-start failures
/// and settlement conflicts never call this function, so mandatory service
/// latency is not fabricated and an uncommitted terminal transition cannot be
/// observed.
pub fn record_work_operation_resource(
    producer: Option<&BoundedObservabilityProducerV1>,
    attempt: &WorkAttemptV1,
    observation: OperationResourceObservedV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Some(observed_at) = attempt.terminal().map(|terminal| terminal.observed_at()) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let scope = producer.identity().authorized_scope_ref.as_str();
    let envelope = match work_operation_resource_observation_envelope(
        producer.identity(),
        scope,
        attempt.identity(),
        attempt.state(),
        observed_at,
        observation,
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

fn work_operation_resource_observation_envelope(
    identity: &ObservabilityProducerIdentityV1,
    canonical_project_scope: &str,
    attempt: &WorkAttemptIdentityV1,
    state: WorkAttemptStateV1,
    observed_at: UtcMicros,
    observation: OperationResourceObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let terminal_result = terminal_result(state).ok_or("work_operation_resource_terminal")?;
    observation.validate(Some(terminal_result))?;
    let owner_transition_ref = format!(
        "work-operation-resource:{}/{}/{}",
        attempt.task_id().as_str(),
        attempt.run_id().as_str(),
        attempt.attempt_id().as_str()
    );
    execution_owner_fact_envelope(
        identity,
        canonical_project_scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: "execute_work_attempt",
            event_time: observed_at,
            valid_from: Some(observed_at),
            valid_until: Some(observed_at),
            terminal_result: Some(terminal_result),
            coverage: CoverageStateV1::Known,
            payload: ObservabilityPayloadV1::OperationResource(Box::new(observation)),
        },
    )
}

const fn terminal_result(state: WorkAttemptStateV1) -> Option<ObservabilityTerminalResultV1> {
    match state {
        WorkAttemptStateV1::Succeeded => Some(ObservabilityTerminalResultV1::Succeeded),
        WorkAttemptStateV1::Failed => Some(ObservabilityTerminalResultV1::Failed),
        WorkAttemptStateV1::TimedOut => Some(ObservabilityTerminalResultV1::TimedOut),
        WorkAttemptStateV1::Cancelled => Some(ObservabilityTerminalResultV1::Cancelled),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        AttemptId, CoverageStateV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
        OperationActivationOutcomeV1, OperationAvailabilityV1, OperationResourceObservedV1,
        OperationStageTimingV1, OperationStageV1, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
        WorkAttemptStateV1,
    };

    use super::work_operation_resource_observation_envelope;
    use crate::observability::ObservabilityProducerIdentityV1;

    fn producer_identity() -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: "project:work-resource".to_owned(),
            process_boot_id: "boot:work-resource".to_owned(),
            producer_revision: "producer.v1".to_owned(),
            configuration_revision: "configuration.v1".to_owned(),
            policy_revision: "policy.v1".to_owned(),
        }
    }

    fn attempt_identity() -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            TaskId::new("task.work-resource".to_owned()).expect("task id"),
            RunId::new("run.work-resource".to_owned()).expect("run id"),
            AttemptId::new("attempt.work-resource".to_owned()).expect("attempt id"),
        )
        .expect("attempt identity")
    }

    fn observation(
        activation_outcome: OperationActivationOutcomeV1,
    ) -> OperationResourceObservedV1 {
        OperationResourceObservedV1 {
            provider_request_id: None,
            scheduled_latency_micros: 5,
            service_latency_micros: 30,
            process_rss_bytes: None,
            process_pss_bytes: None,
            cpu_user_micros: None,
            cpu_system_micros: None,
            read_bytes: None,
            write_bytes: None,
            input_tokens: None,
            output_tokens: None,
            cost_amount: None,
            cost_currency: None,
            pricing_revision: None,
            stage_timings: vec![
                OperationStageTimingV1 {
                    stage: OperationStageV1::Scheduled,
                    elapsed_micros: 0,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Admitted,
                    elapsed_micros: 5,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Started,
                    elapsed_micros: 10,
                },
                OperationStageTimingV1 {
                    stage: OperationStageV1::Terminal,
                    elapsed_micros: 40,
                },
            ],
            phase_timings: Vec::new(),
            absolute_deadline_micros: Some(1_000),
            availability: OperationAvailabilityV1::Available,
            activation_outcome: Some(activation_outcome),
            process_count: None,
            input_bytes: None,
            output_bytes: None,
        }
    }

    #[test]
    fn settled_attempt_resource_replay_keeps_owner_identity_and_exact_payload() {
        let identity = producer_identity();
        let attempt = attempt_identity();
        let first = work_operation_resource_observation_envelope(
            &identity,
            "project:work-resource",
            &attempt,
            WorkAttemptStateV1::Succeeded,
            UtcMicros(900),
            observation(OperationActivationOutcomeV1::Committed),
        )
        .expect("settled resource observation");
        let replay = work_operation_resource_observation_envelope(
            &identity,
            "project:work-resource",
            &attempt,
            WorkAttemptStateV1::Succeeded,
            UtcMicros(900),
            observation(OperationActivationOutcomeV1::Committed),
        )
        .expect("settled resource replay");

        assert_eq!(first, replay);
        assert_eq!(
            first.terminal_result,
            Some(ObservabilityTerminalResultV1::Succeeded)
        );
        assert_eq!(first.coverage, CoverageStateV1::Known);
        let ObservabilityPayloadV1::OperationResource(resource) = first.payload else {
            panic!("expected operation resource payload");
        };
        assert_eq!(resource.scheduled_latency_micros, 5);
        assert_eq!(resource.service_latency_micros, 30);
        assert_eq!(resource.provider_request_id, None);
    }

    #[test]
    fn nonterminal_attempt_cannot_publish_a_terminal_resource_receipt() {
        assert_eq!(
            work_operation_resource_observation_envelope(
                &producer_identity(),
                "project:work-resource",
                &attempt_identity(),
                WorkAttemptStateV1::Running,
                UtcMicros(900),
                observation(OperationActivationOutcomeV1::Admitted),
            ),
            Err("work_operation_resource_terminal")
        );
    }
}
