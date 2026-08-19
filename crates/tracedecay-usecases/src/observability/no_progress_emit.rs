//! Terminal no-progress receipts for wall-exhausted Work provider attempts.
//!
//! The provider-attempt authority commits no intermediate progress frontier
//! (`Leased -> Running -> terminal`, heartbeats never reset the deadline), so
//! at the wall-exhaustion kill the frontier is provably zero, the stall is
//! the measured wall since the attempt began, no run budget remains, and the
//! unreconciled worktree effect outcome is truthfully unknown.

use tracedecay_domain::{
    CoverageStateV1, EffectReconciliationOutcomeV1, NoProgressEscalationV1, NoProgressObservedV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1, UtcMicros,
    WorkAttemptIdentityV1, WorkflowStageClassV1, canonical_sha256,
};

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerIdentityV1, WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

/// One wall-exhaustion kill measured by the live attempt-execution owner.
/// Every field is a value the owner actually holds at the kill site.
pub struct WorkNoProgressObservationV1<'a> {
    pub attempt: &'a WorkAttemptIdentityV1,
    pub run_deadline: UtcMicros,
    pub concurrency_policy_revision: &'a str,
    pub configured_timeout_micros: u64,
    pub elapsed_stall_micros: u64,
    pub observed_at: UtcMicros,
}

/// Offers one no-progress terminal fact without awaiting telemetry. The
/// payload contract refuses a zero wall budget and a stall shorter than the
/// armed budget; emission never changes the timed-out product handling.
pub fn record_no_progress_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    observation: &WorkNoProgressObservationV1<'_>,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let scope = producer.identity().authorized_scope_ref.as_str();
    let envelope = match no_progress_observation_envelope(producer.identity(), scope, observation) {
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

/// Builds the canonical owner envelope for one wall-exhaustion kill. The
/// run-deadline reference hashes the exact attempt identity and deadline, so
/// replays are idempotent and raw identifiers never enter the payload.
fn no_progress_observation_envelope(
    identity: &ObservabilityProducerIdentityV1,
    canonical_project_scope: &str,
    observation: &WorkNoProgressObservationV1<'_>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let run_deadline_digest = canonical_sha256(&(
        "tracedecay.work.run-deadline.v1",
        observation.attempt.task_id().as_str(),
        observation.attempt.run_id().as_str(),
        observation.attempt.attempt_id().as_str(),
        observation.run_deadline,
    ))
    .map_err(|_| "no_progress_run_deadline_identity")?;
    let payload = NoProgressObservedV1 {
        run_deadline_ref: format!("work-run-deadline:{}", run_deadline_digest.as_str()),
        concurrency_policy_revision: observation.concurrency_policy_revision.to_owned(),
        workflow_stage: WorkflowStageClassV1::Execute,
        configured_timeout_micros: observation.configured_timeout_micros,
        // The provider-attempt authority has no frontier commits between
        // Running and the terminal transition; zero is the proven frontier,
        // not a default.
        last_committed_frontier: 0,
        elapsed_stall_micros: observation.elapsed_stall_micros,
        // The armed wall budget is the envelope deadline itself; at
        // exhaustion no run budget remains above it.
        remaining_run_budget_micros: 0,
        // Both live timeout sites deliver SIGKILL to the provider's whole
        // process group/tree with no graceful rung.
        escalation: NoProgressEscalationV1::Kill,
        effect_outcome: EffectReconciliationOutcomeV1::Unknown,
    };
    payload.validate()?;
    let owner_transition_ref = format!(
        "work-no-progress:{}/{}/{}",
        observation.attempt.task_id().as_str(),
        observation.attempt.run_id().as_str(),
        observation.attempt.attempt_id().as_str()
    );
    execution_owner_fact_envelope(
        identity,
        canonical_project_scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &owner_transition_ref,
            operation: "execute_work_attempt",
            event_time: observation.observed_at,
            valid_from: Some(observation.observed_at),
            valid_until: Some(observation.observed_at),
            terminal_result: Some(ObservabilityTerminalResultV1::TimedOut),
            coverage: CoverageStateV1::Known,
            payload: ObservabilityPayloadV1::NoProgress(payload),
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{ObservabilityQueryPort, ObservabilityQueryV1};
    use tracedecay_domain::{AttemptId, RunId, TaskId};

    use crate::observability::RegisteredObservabilityPortV1;

    const SCOPE: &str = "project.no-progress-emit";

    fn producer_identity() -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: SCOPE.to_owned(),
            process_boot_id: "boot:no-progress-emit".to_owned(),
            producer_revision: "producer.v1".to_owned(),
            configuration_revision: "configuration.v1".to_owned(),
            policy_revision: "policy.v1".to_owned(),
        }
    }

    fn attempt_identity() -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            TaskId::new("task.no-progress".to_owned()).expect("task id"),
            RunId::new("run.no-progress".to_owned()).expect("run id"),
            AttemptId::new("attempt.no-progress".to_owned()).expect("attempt id"),
        )
        .expect("attempt identity")
    }

    fn observation(
        attempt: &WorkAttemptIdentityV1,
        configured_timeout_micros: u64,
        elapsed_stall_micros: u64,
    ) -> WorkNoProgressObservationV1<'_> {
        WorkNoProgressObservationV1 {
            attempt,
            run_deadline: UtcMicros(2_000_000),
            concurrency_policy_revision: "topology-policy.v1",
            configured_timeout_micros,
            elapsed_stall_micros,
            observed_at: UtcMicros(1_550_000),
        }
    }

    #[tokio::test]
    async fn wall_exhausted_attempt_persists_the_no_progress_terminal_fact() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "no-progress-emit",
        )
        .await;
        let producer = BoundedObservabilityProducerV1::start(
            harness.registered.clone(),
            producer_identity(),
            8,
        )
        .expect("bounded producer");
        let attempt = attempt_identity();

        assert_eq!(
            record_no_progress_observation(
                Some(&producer),
                &observation(&attempt, 30_000_000, 31_000_000),
            ),
            WorkOwnerObservationResultV1::Enqueued
        );
        // A stall shorter than the armed budget is refused, not persisted.
        assert_eq!(
            record_no_progress_observation(
                Some(&producer),
                &observation(&attempt, 30_000_000, 29_000_000),
            ),
            WorkOwnerObservationResultV1::Unavailable
        );
        producer.shutdown().await.expect("producer shutdown");

        let page = RegisteredObservabilityPortV1::new(&harness.registered)
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: SCOPE.to_owned(),
                event_kinds: vec!["operation.no_progress.terminal.v1".to_owned()],
                horizon: tracedecay_application::ObservabilityHorizonV1 {
                    since_micros: 1_500_000,
                    until_micros: 1_600_000,
                },
                after_watermark: None,
                limit: 8,
            })
            .await
            .expect("no-progress page");
        assert_eq!(page.events.len(), 1);
        let envelope = &page.events[0];
        assert_eq!(envelope.event_kind, "operation.no_progress.terminal.v1");
        assert_eq!(
            envelope.terminal_result,
            Some(ObservabilityTerminalResultV1::TimedOut)
        );
        assert_eq!(envelope.coverage, CoverageStateV1::Known);
        assert_eq!(envelope.event_time_micros, 1_550_000);
        let ObservabilityPayloadV1::NoProgress(observed) = &envelope.payload else {
            panic!("expected a no-progress payload, got {:?}", envelope.payload);
        };
        let expected_deadline_ref = format!(
            "work-run-deadline:{}",
            canonical_sha256(&(
                "tracedecay.work.run-deadline.v1",
                attempt.task_id().as_str(),
                attempt.run_id().as_str(),
                attempt.attempt_id().as_str(),
                UtcMicros(2_000_000),
            ))
            .expect("run deadline digest")
            .as_str()
        );
        assert_eq!(observed.run_deadline_ref, expected_deadline_ref);
        assert_eq!(observed.concurrency_policy_revision, "topology-policy.v1");
        assert_eq!(observed.workflow_stage, WorkflowStageClassV1::Execute);
        assert_eq!(observed.configured_timeout_micros, 30_000_000);
        assert_eq!(observed.last_committed_frontier, 0);
        assert_eq!(observed.elapsed_stall_micros, 31_000_000);
        assert_eq!(observed.remaining_run_budget_micros, 0);
        assert_eq!(observed.escalation, NoProgressEscalationV1::Kill);
        assert_eq!(
            observed.effect_outcome,
            EffectReconciliationOutcomeV1::Unknown
        );
        // Raw attempt identifiers never enter the exportable envelope.
        let wire = serde_json::to_string(envelope).expect("serialize envelope");
        for prohibited in ["task.no-progress", "run.no-progress", "attempt.no-progress"] {
            assert!(!wire.contains(prohibited), "leaked {prohibited}");
        }
    }

    #[test]
    fn absent_producer_is_a_typed_unavailable_state() {
        let attempt = attempt_identity();
        assert_eq!(
            record_no_progress_observation(None, &observation(&attempt, 30_000_000, 31_000_000)),
            WorkOwnerObservationResultV1::Unavailable
        );
    }

    #[test]
    fn unmeasured_or_invalid_inputs_are_refused_without_panicking() {
        let identity = producer_identity();
        let attempt = attempt_identity();
        // A zero wall budget is the deadline-already-elapsed admission state,
        // not a measured stall.
        assert_eq!(
            no_progress_observation_envelope(&identity, SCOPE, &observation(&attempt, 0, 0)),
            Err("no_progress_timeout")
        );
        // A stall shorter than the armed budget was not a timeout.
        assert_eq!(
            no_progress_observation_envelope(
                &identity,
                SCOPE,
                &observation(&attempt, 30_000_000, 29_999_999),
            ),
            Err("no_progress_timeout")
        );
        // A non-canonical concurrency-policy revision is refused.
        let oversized_revision = "r".repeat(97);
        let mut invalid = observation(&attempt, 30_000_000, 31_000_000);
        invalid.concurrency_policy_revision = &oversized_revision;
        assert_eq!(
            no_progress_observation_envelope(&identity, SCOPE, &invalid),
            Err("revision")
        );
    }

    #[test]
    fn replayed_kill_builds_byte_identical_idempotent_envelopes() {
        let identity = producer_identity();
        let attempt = attempt_identity();
        let first = no_progress_observation_envelope(
            &identity,
            SCOPE,
            &observation(&attempt, 30_000_000, 31_000_000),
        )
        .expect("first envelope");
        let replay = no_progress_observation_envelope(
            &identity,
            SCOPE,
            &observation(&attempt, 30_000_000, 31_000_000),
        )
        .expect("replay envelope");
        assert_eq!(first, replay, "owner-measured kill replays byte-identical");
    }
}
