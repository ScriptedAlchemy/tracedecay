//! Workflow observability derived from the durable run journal and census.

use tracedecay_domain::{
    CoverageStateV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkflowCensusDurationV1, WorkflowFanOutCensusV1, WorkflowLifecycleObservedV1,
    WorkflowOutcomeObservedV1, WorkflowResourceObservedV1, WorkflowRunProjection,
    WorkflowRunStatus, WorkflowStepStatus,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

/// Records one immutable Workflow journal/census settlement. Lifecycle facts
/// track the journal; outcome and final resource facts exist only after the
/// owning Workflow journal reaches a terminal state.
pub fn record_workflow_settlement(
    producer: Option<&BoundedObservabilityProducerV1>,
    projection: &WorkflowRunProjection,
    census: &WorkflowFanOutCensusV1,
    attempts: &[WorkAttemptV1],
    attempt_reads_complete: bool,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    if census.validate().is_err()
        || census.run_id != *projection.run_id()
        || census.workflow_sequence != projection.sequence()
        || census.topology_digest != *projection.pinned_topology_digest()
        || census.provider_registry_digest != *projection.pinned_provider_registry_digest()
        || producer.identity().authorized_scope_ref != projection.definition().project_id().as_str()
    {
        return WorkOwnerObservationResultV1::Unavailable;
    }
    let Some(started_at) = projection
        .history()
        .first()
        .map(|event| event.occurred_at())
    else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Ok(total_steps) = u32::try_from(projection.definition().steps().len()) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let lifecycle = WorkflowLifecycleObservedV1 {
        run_id: projection.run_id().clone(),
        workflow_sequence: projection.sequence(),
        definition_ref: projection.definition().definition_id().as_str().to_owned(),
        definition_version: projection.definition().definition_version(),
        topology_digest: projection.pinned_topology_digest().clone(),
        provider_registry_digest: projection.pinned_provider_registry_digest().clone(),
        status: projection.status(),
        started_at,
        observed_at: census.observed_at,
        total_steps,
        coverage: CoverageStateV1::Known,
    };
    let planned_child_count = projection
        .fan_out_plans()
        .values()
        .map(|plan| plan.children.len())
        .try_fold(0_usize, usize::checked_add);
    let Some(planned_child_count) = planned_child_count else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let planned_attempts = projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| plan.children.iter().map(|child| &child.attempt_identity))
        .collect::<std::collections::BTreeSet<_>>();
    let observed_attempt_identities = attempts
        .iter()
        .map(WorkAttemptV1::identity)
        .collect::<std::collections::BTreeSet<_>>();
    if planned_attempts.len() != planned_child_count
        || observed_attempt_identities.len() != attempts.len()
        || !observed_attempt_identities.is_subset(&planned_attempts)
        || census
            .attempt_frontiers
            .iter()
            .any(|frontier| !planned_attempts.contains(&frontier.attempt))
    {
        return WorkOwnerObservationResultV1::Unavailable;
    }
    let Ok(eligible_attempts) = u32::try_from(planned_child_count) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Ok(observed_attempts) = u32::try_from(attempts.len()) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    if observed_attempts > eligible_attempts {
        return WorkOwnerObservationResultV1::Unavailable;
    }
    let artifact_count = attempts.iter().try_fold(0_u64, |count, attempt| {
        u64::try_from(attempt.artifacts().len())
            .ok()
            .and_then(|artifacts| count.checked_add(artifacts))
    });
    let Some(artifact_count) = artifact_count else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let observed_duration_micros = known_duration(&census.observed_duration);
    let critical_path_duration_micros = known_duration(&census.critical_path_duration);
    let resource_coverage = if attempt_reads_complete
        && observed_attempts == eligible_attempts
        && observed_duration_micros.is_some()
        && critical_path_duration_micros.is_some()
    {
        CoverageStateV1::Known
    } else if observed_attempts > 0
        || observed_duration_micros.is_some()
        || critical_path_duration_micros.is_some()
    {
        CoverageStateV1::Partial
    } else {
        CoverageStateV1::Unknown
    };
    let resource = WorkflowResourceObservedV1 {
        run_id: projection.run_id().clone(),
        workflow_sequence: projection.sequence(),
        eligible_attempts,
        observed_attempts,
        artifact_count,
        observed_duration_micros,
        critical_path_duration_micros,
        coverage: resource_coverage,
    };
    let terminal = terminal_result(projection.status());
    let mut payloads = vec![(
        "lifecycle",
        CoverageStateV1::Known,
        ObservabilityPayloadV1::WorkflowLifecycle(lifecycle),
    )];
    if terminal.is_some() {
        payloads.push((
            "resource",
            resource_coverage,
            ObservabilityPayloadV1::WorkflowResource(resource),
        ));
        let mut succeeded_steps = 0_u32;
        let mut failed_steps = 0_u32;
        let mut cancelled_steps = 0_u32;
        let mut unknown_steps = 0_u32;
        for step in projection.definition().steps() {
            match projection.step(&step.step_id).map(|value| value.status()) {
                Some(WorkflowStepStatus::Succeeded) => succeeded_steps += 1,
                Some(WorkflowStepStatus::Failed) => failed_steps += 1,
                Some(WorkflowStepStatus::Cancelled) => cancelled_steps += 1,
                Some(
                    WorkflowStepStatus::Blocked
                    | WorkflowStepStatus::Ready
                    | WorkflowStepStatus::Running,
                )
                | None => unknown_steps += 1,
            }
        }
        let Some(succeeded_attempts) = count_attempts(attempts, WorkAttemptStateV1::Succeeded)
        else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let Some(failed_attempts) = count_attempts(attempts, WorkAttemptStateV1::Failed) else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let Some(timed_out_attempts) = count_attempts(attempts, WorkAttemptStateV1::TimedOut)
        else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let Some(cancelled_attempts) = count_attempts(attempts, WorkAttemptStateV1::Cancelled)
        else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let Some(classified_attempts) = succeeded_attempts
            .checked_add(failed_attempts)
            .and_then(|value| value.checked_add(timed_out_attempts))
            .and_then(|value| value.checked_add(cancelled_attempts))
        else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let Some(unknown_attempts) = eligible_attempts.checked_sub(classified_attempts) else {
            return WorkOwnerObservationResultV1::Unavailable;
        };
        let outcome_coverage =
            if attempt_reads_complete && unknown_steps == 0 && unknown_attempts == 0 {
                CoverageStateV1::Known
            } else {
                CoverageStateV1::Partial
            };
        let outcome = WorkflowOutcomeObservedV1 {
            run_id: projection.run_id().clone(),
            workflow_sequence: projection.sequence(),
            status: projection.status(),
            total_steps,
            succeeded_steps,
            failed_steps,
            cancelled_steps,
            unknown_steps,
            eligible_attempts,
            observed_attempts: classified_attempts,
            succeeded_attempts,
            failed_attempts,
            timed_out_attempts,
            cancelled_attempts,
            unknown_attempts,
            coverage: outcome_coverage,
        };
        payloads.push((
            "outcome",
            outcome_coverage,
            ObservabilityPayloadV1::WorkflowOutcome(outcome),
        ));
    }
    let envelopes = payloads
        .into_iter()
        .map(|(kind, coverage, payload)| {
            execution_owner_fact_envelope(
                producer.identity(),
                projection.definition().project_id().as_str(),
                ExecutionOwnerFactInputV1 {
                    owner_transition_ref: &format!(
                        "workflow-settlement:{}:{}:{kind}",
                        projection.run_id().as_str(),
                        projection.sequence()
                    ),
                    operation: "workflow_settlement",
                    event_time: census.observed_at,
                    valid_from: Some(started_at),
                    valid_until: Some(census.observed_at),
                    terminal_result: terminal,
                    coverage,
                    payload,
                },
            )
        })
        .collect::<Result<Vec<_>, _>>();
    let Ok(envelopes) = envelopes else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    match producer.try_emit_owner_facts(envelopes) {
        Ok(outcomes)
            if outcomes
                .iter()
                .all(|outcome| *outcome == ObservabilityEmissionOutcomeV1::Enqueued) =>
        {
            WorkOwnerObservationResultV1::Enqueued
        }
        Ok(_) => WorkOwnerObservationResultV1::DroppedAtCapacity,
        Err(_) => WorkOwnerObservationResultV1::Unavailable,
    }
}

const fn known_duration(duration: &WorkflowCensusDurationV1) -> Option<u64> {
    match duration {
        WorkflowCensusDurationV1::Known { micros } => Some(*micros),
        WorkflowCensusDurationV1::Partial { .. } | WorkflowCensusDurationV1::Unavailable { .. } => {
            None
        }
    }
}

fn count_attempts(attempts: &[WorkAttemptV1], state: WorkAttemptStateV1) -> Option<u32> {
    attempts
        .iter()
        .filter(|attempt| attempt.state() == state)
        .count()
        .try_into()
        .ok()
}

const fn terminal_result(status: WorkflowRunStatus) -> Option<ObservabilityTerminalResultV1> {
    match status {
        WorkflowRunStatus::Completed => Some(ObservabilityTerminalResultV1::Succeeded),
        WorkflowRunStatus::Failed => Some(ObservabilityTerminalResultV1::Failed),
        WorkflowRunStatus::Cancelled => Some(ObservabilityTerminalResultV1::Cancelled),
        WorkflowRunStatus::Running | WorkflowRunStatus::Paused | WorkflowRunStatus::Cancelling => {
            None
        }
    }
}
