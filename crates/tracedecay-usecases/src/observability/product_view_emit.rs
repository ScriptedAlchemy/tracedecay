//! Product-view observations emitted only from exact Work owner results.

use tracedecay_application::{GeneratedWorkProposal, ReviewProposalDispositionV1};
use tracedecay_domain::{
    AppropriateRelianceObservedV1, AutomationFunnelObservedV1, AutomationTerminalV1,
    CoverageStateV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1, ObservedTernaryV1,
    ProviderAttemptTerminalV1, ProviderReliabilityObservedV1, RelianceDecisionV1,
    RelianceVerificationV1, RemoteCoverageObservedV1, TaskCalibrationEvidenceV1,
    TaskDecisionDispositionV1, TaskIntelligenceDecisionObservedV1,
    TaskIntelligenceOutcomeObservedV1, TaskOutcomeV1, UtcMicros, WorkAttemptStateV1, WorkAttemptV1,
    WorkCancellationStateV1, WorkProviderBackendV1, WorkProviderProtocol, WorkRecoveryStateV1,
};
use tracedecay_policy::work_loop::WorkProposalDispositionV1;

use super::{
    BoundedObservabilityProducerV1, ExecutionOwnerFactInputV1, ObservabilityEmissionOutcomeV1,
    WorkOwnerObservationResultV1, execution_owner_fact_envelope,
};

pub fn record_task_intelligence_decision(
    producer: Option<&BoundedObservabilityProducerV1>,
    proposal: &GeneratedWorkProposal,
    observed_at: UtcMicros,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let decision = &proposal.decision;
    let decomposition_candidate_count = decision
        .decomposition
        .as_ref()
        .and_then(|value| u32::try_from(value.candidates.len()).ok());
    let route_candidate_count = decision.route_plan.as_ref().and_then(|value| {
        u32::try_from(value.ranked.len().saturating_add(value.exclusions.len())).ok()
    });
    let payload =
        ObservabilityPayloadV1::TaskIntelligenceDecision(TaskIntelligenceDecisionObservedV1 {
            proposal_ref: proposal.proposal.proposal_id().as_str().to_owned(),
            task_ref: proposal.proposal.task_id().as_str().to_owned(),
            evaluator_revision: decision.evaluator_revision,
            disposition: match decision.disposition {
                WorkProposalDispositionV1::Allow => TaskDecisionDispositionV1::Allow,
                WorkProposalDispositionV1::Deny => TaskDecisionDispositionV1::Deny,
                WorkProposalDispositionV1::Abstain => TaskDecisionDispositionV1::Abstain,
                WorkProposalDispositionV1::Indeterminate => {
                    TaskDecisionDispositionV1::Indeterminate
                }
            },
            deterministic_fallback: decision.deterministic_fallback,
            calibration: decision
                .sizing
                .as_ref()
                .map(|value| TaskCalibrationEvidenceV1 {
                    cohort_ref: value.cohort.clone(),
                    support: value.support,
                    support_floor: value.support_floor,
                    drift_valid: value.drift_valid,
                }),
            decomposition_candidate_count,
            route_candidate_count,
        });
    emit(
        producer,
        &format!("work-proposal:{}", proposal.proposal.proposal_id().as_str()),
        "generate_proposal",
        observed_at,
        Some(ObservabilityTerminalResultV1::Succeeded),
        payload,
    )
}

pub fn record_automation_funnel_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    observation: AutomationFunnelObservedV1,
    observed_at: UtcMicros,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let owner_ref = format!(
        "automation-run:{}:{}:{}:{}",
        observation.run_ref,
        automation_terminal_name(observation.terminal),
        observed_at.0,
        coverage_name(observation.ledger_coverage)
    );
    let coverage = observation.ledger_coverage;
    emit_with_coverage(
        producer,
        &owner_ref,
        "automation_run_lifecycle",
        observed_at,
        observation
            .terminal
            .is_terminal()
            .then_some(automation_terminal_result(observation.terminal)),
        ObservabilityPayloadV1::AutomationFunnel(observation),
        coverage,
    )
}

pub fn record_reliance_decision(
    producer: Option<&BoundedObservabilityProducerV1>,
    proposal_ref: &str,
    command_ref: &str,
    disposition: Option<ReviewProposalDispositionV1>,
    observed_at: UtcMicros,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let decision = match disposition {
        None => RelianceDecisionV1::Accepted,
        Some(ReviewProposalDispositionV1::Rejected) => RelianceDecisionV1::Rejected,
        // Superseding does not carry the rationale required to classify an
        // override, so it is deliberately not projected as reliance.
        Some(ReviewProposalDispositionV1::Superseded) => {
            return WorkOwnerObservationResultV1::Unavailable;
        }
    };
    emit(
        producer,
        &format!("work-reliance:{command_ref}"),
        "review_proposal",
        observed_at,
        Some(ObservabilityTerminalResultV1::Succeeded),
        ObservabilityPayloadV1::AppropriateReliance(AppropriateRelianceObservedV1 {
            decision_ref: proposal_ref.to_owned(),
            decision,
            // Review/acceptance proves reliance, not correctness. An
            // independently linked outcome can supersede this state later.
            verification: RelianceVerificationV1::NoEligibleVerification,
            independently_verified: false,
            override_rationale_present: false,
        }),
    )
}

pub fn record_remote_coverage_observation(
    producer: Option<&BoundedObservabilityProducerV1>,
    observation: RemoteCoverageObservedV1,
    observed_at: UtcMicros,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let terminal_result = match observation.terminal_succeeded {
        ObservedTernaryV1::Yes => ObservabilityTerminalResultV1::Succeeded,
        ObservedTernaryV1::No => ObservabilityTerminalResultV1::Failed,
        ObservedTernaryV1::Unknown => ObservabilityTerminalResultV1::Unknown,
    };
    emit(
        producer,
        &format!("remote-coverage:{}", observation.operation_ref),
        "remote_protocol",
        observed_at,
        Some(terminal_result),
        ObservabilityPayloadV1::RemoteCoverage(observation),
    )
}

pub fn record_terminal_attempt_product_views(
    producer: Option<&BoundedObservabilityProducerV1>,
    attempt: &WorkAttemptV1,
) -> WorkOwnerObservationResultV1 {
    let Some(producer) = producer else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let Some(terminal) = terminal(attempt.state()) else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    let observed_at = match attempt.terminal() {
        Some(value) => value.observed_at(),
        None => return WorkOwnerObservationResultV1::Unavailable,
    };
    let attempt_ref = attempt.identity().attempt_id().as_str().to_owned();
    let proposal_ref = attempt
        .projection_binding()
        .accepted_proposal()
        .as_str()
        .to_owned();
    let snapshot = attempt.execution().execution_snapshot();
    let fallback = match attempt.actual_route() {
        Some(actual) if actual == attempt.requested_route() => ObservedTernaryV1::No,
        Some(_) => ObservedTernaryV1::Yes,
        None => ObservedTernaryV1::Unknown,
    };
    let provider = ObservabilityPayloadV1::ProviderReliability(ProviderReliabilityObservedV1 {
        attempt_ref: attempt_ref.clone(),
        backend: backend(snapshot.backend()).to_owned(),
        protocol: protocol(snapshot.protocol()).to_owned(),
        model: Some(snapshot.model().to_owned()),
        fallback,
        progress: if attempt.progress().is_some() {
            ObservedTernaryV1::Yes
        } else {
            ObservedTernaryV1::No
        },
        cancellation: if matches!(attempt.cancellation(), WorkCancellationStateV1::None) {
            ObservedTernaryV1::No
        } else {
            ObservedTernaryV1::Yes
        },
        recovery: if matches!(attempt.recovery(), WorkRecoveryStateV1::Fresh) {
            ObservedTernaryV1::No
        } else {
            ObservedTernaryV1::Yes
        },
        artifact_count: match u32::try_from(attempt.artifacts().len()) {
            Ok(value) => value,
            Err(_) => return WorkOwnerObservationResultV1::Unavailable,
        },
        terminal,
        // The effect receipt is a separate authority and is not currently
        // correlated on attempt identity at this read point.
        effect: ObservedTernaryV1::Unknown,
        input_tokens: None,
        output_tokens: None,
        cost_amount: None,
        cost_currency: None,
        usage_coverage: CoverageStateV1::Unknown,
        usage_unavailable_reason: Some("provider_usage_not_correlated_to_work_attempt".to_owned()),
    });
    let outcome =
        ObservabilityPayloadV1::TaskIntelligenceOutcome(TaskIntelligenceOutcomeObservedV1 {
            proposal_ref,
            attempt_ref: attempt_ref.clone(),
            outcome: task_outcome(terminal),
            independently_reviewed: ObservedTernaryV1::Unknown,
            accepted: ObservedTernaryV1::Unknown,
            effect: ObservedTernaryV1::Unknown,
        });
    let scope = producer.identity().authorized_scope_ref.as_str();
    let provider_envelope = execution_owner_fact_envelope(
        producer.identity(),
        scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &format!("work-provider:{attempt_ref}"),
            operation: "execute_work_attempt",
            event_time: observed_at,
            valid_from: None,
            valid_until: Some(observed_at),
            terminal_result: Some(terminal_result(terminal)),
            coverage: CoverageStateV1::Known,
            payload: provider,
        },
    );
    let outcome_envelope = execution_owner_fact_envelope(
        producer.identity(),
        scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: &format!("work-outcome:{attempt_ref}"),
            operation: "execute_work_attempt",
            event_time: observed_at,
            valid_from: None,
            valid_until: Some(observed_at),
            terminal_result: Some(terminal_result(terminal)),
            coverage: CoverageStateV1::Known,
            payload: outcome,
        },
    );
    let (Ok(provider_envelope), Ok(outcome_envelope)) = (provider_envelope, outcome_envelope)
    else {
        return WorkOwnerObservationResultV1::Unavailable;
    };
    match producer.try_emit_owner_facts(vec![provider_envelope, outcome_envelope]) {
        Ok(outcomes)
            if outcomes
                .iter()
                .all(|value| *value == ObservabilityEmissionOutcomeV1::Enqueued) =>
        {
            WorkOwnerObservationResultV1::Enqueued
        }
        Ok(_) => WorkOwnerObservationResultV1::DroppedAtCapacity,
        Err(_) => WorkOwnerObservationResultV1::Unavailable,
    }
}

fn emit(
    producer: &BoundedObservabilityProducerV1,
    owner_ref: &str,
    operation: &str,
    observed_at: UtcMicros,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    payload: ObservabilityPayloadV1,
) -> WorkOwnerObservationResultV1 {
    emit_with_coverage(
        producer,
        owner_ref,
        operation,
        observed_at,
        terminal_result,
        payload,
        CoverageStateV1::Known,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_with_coverage(
    producer: &BoundedObservabilityProducerV1,
    owner_ref: &str,
    operation: &str,
    observed_at: UtcMicros,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    payload: ObservabilityPayloadV1,
    coverage: CoverageStateV1,
) -> WorkOwnerObservationResultV1 {
    let scope = producer.identity().authorized_scope_ref.as_str();
    let envelope = execution_owner_fact_envelope(
        producer.identity(),
        scope,
        ExecutionOwnerFactInputV1 {
            owner_transition_ref: owner_ref,
            operation,
            event_time: observed_at,
            valid_from: None,
            valid_until: Some(observed_at),
            terminal_result,
            coverage,
            payload,
        },
    );
    let Ok(envelope) = envelope else {
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

const fn terminal(state: WorkAttemptStateV1) -> Option<ProviderAttemptTerminalV1> {
    match state {
        WorkAttemptStateV1::Succeeded => Some(ProviderAttemptTerminalV1::Succeeded),
        WorkAttemptStateV1::Failed => Some(ProviderAttemptTerminalV1::Failed),
        WorkAttemptStateV1::TimedOut => Some(ProviderAttemptTerminalV1::TimedOut),
        WorkAttemptStateV1::Cancelled => Some(ProviderAttemptTerminalV1::Cancelled),
        _ => None,
    }
}

const fn task_outcome(terminal: ProviderAttemptTerminalV1) -> TaskOutcomeV1 {
    match terminal {
        ProviderAttemptTerminalV1::Succeeded => TaskOutcomeV1::Succeeded,
        ProviderAttemptTerminalV1::Failed => TaskOutcomeV1::Failed,
        ProviderAttemptTerminalV1::TimedOut => TaskOutcomeV1::TimedOut,
        ProviderAttemptTerminalV1::Cancelled => TaskOutcomeV1::Cancelled,
    }
}

const fn terminal_result(terminal: ProviderAttemptTerminalV1) -> ObservabilityTerminalResultV1 {
    match terminal {
        ProviderAttemptTerminalV1::Succeeded => ObservabilityTerminalResultV1::Succeeded,
        ProviderAttemptTerminalV1::Failed => ObservabilityTerminalResultV1::Failed,
        ProviderAttemptTerminalV1::TimedOut => ObservabilityTerminalResultV1::TimedOut,
        ProviderAttemptTerminalV1::Cancelled => ObservabilityTerminalResultV1::Cancelled,
    }
}

const fn backend(value: WorkProviderBackendV1) -> &'static str {
    match value {
        WorkProviderBackendV1::ClaudeCodeCli => "claude_code_cli",
        WorkProviderBackendV1::CodexAppServer => "codex_app_server",
        WorkProviderBackendV1::CodexCli => "codex_cli",
    }
}

const fn protocol(value: WorkProviderProtocol) -> &'static str {
    match value {
        WorkProviderProtocol::ClaudeStreamJson => "claude_stream_json",
        WorkProviderProtocol::CodexAppServerJsonRpc => "codex_app_server_json_rpc",
        WorkProviderProtocol::CodexExecJson => "codex_exec_json",
    }
}

const fn automation_terminal_name(value: AutomationTerminalV1) -> &'static str {
    match value {
        AutomationTerminalV1::Succeeded => "succeeded",
        AutomationTerminalV1::Failed => "failed",
        AutomationTerminalV1::Skipped => "skipped",
        AutomationTerminalV1::Running => "running",
        AutomationTerminalV1::Queued => "queued",
    }
}

const fn automation_terminal_result(value: AutomationTerminalV1) -> ObservabilityTerminalResultV1 {
    match value {
        AutomationTerminalV1::Succeeded => ObservabilityTerminalResultV1::Succeeded,
        AutomationTerminalV1::Failed => ObservabilityTerminalResultV1::Failed,
        AutomationTerminalV1::Skipped => ObservabilityTerminalResultV1::Abstained,
        AutomationTerminalV1::Running | AutomationTerminalV1::Queued => {
            ObservabilityTerminalResultV1::Unknown
        }
    }
}

const fn coverage_name(value: CoverageStateV1) -> &'static str {
    match value {
        CoverageStateV1::Known => "known",
        CoverageStateV1::Partial => "partial",
        CoverageStateV1::Stale => "stale",
        CoverageStateV1::Unknown => "unknown",
        CoverageStateV1::Sampled => "sampled",
        CoverageStateV1::Capped => "capped",
    }
}
