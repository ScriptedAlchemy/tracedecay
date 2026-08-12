//! Canonical admitted problem mapping for automatic-memory runs.

use tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord;
use tracedecay_application::retained_surfaces::{
    MemoryAutomationRunProblemV1, MemoryAutomationRunRequestV1,
};
use tracedecay_application::{
    ApplicationExecutionFailureClassV1, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationUnavailableClassV1, CancellationSignal, CancellationStage, LegalAction,
    ProblemOwningLayer, RequestAdmission, RequestContext, RetryDirective, SafeDiagnostic,
};
use tracedecay_automation::backend::AgentTaskFailureClass;

use super::{AutomationSettledProblem, contract_error};
use crate::errors::{Result, TraceDecayError};

pub(super) fn reset_required_problem(
    operation: &tracedecay_application::ApplicationOperation,
    context: &RequestContext,
    request: &MemoryAutomationRunRequestV1,
) -> Result<AutomationSettledProblem> {
    zero_effect_terminal(
        operation,
        context,
        request,
        ApplicationProblem::ResetRequired {
            diagnostic: SafeDiagnostic::new(
                "application.memory-automation-run.reset-required",
                "No canonical memory effect was found for the interrupted automatic-memory run; reset its exact run identity before reuse.",
            )
            .map_err(contract_error)?,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reset],
        },
    )
}

pub(super) fn shipped_proposal_reset_required_problem(
    operation: &tracedecay_application::ApplicationOperation,
    context: &RequestContext,
    request: &MemoryAutomationRunRequestV1,
) -> Result<AutomationSettledProblem> {
    zero_effect_terminal(
        operation,
        context,
        request,
        ApplicationProblem::ResetRequired {
            diagnostic: SafeDiagnostic::new(
                "application.memory-automation-run.shipped-proposals-reset-required",
                "Unresolved shipped fact-proposal state cannot be imported because final-V2 has no approval authority; preserve it and explicitly reset its exact file.",
            )
            .map_err(contract_error)?,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reset],
        },
    )
}

pub(super) fn failed_ledger_problem(
    context: &RequestContext,
    cancellation: &CancellationSignal,
    ledger: &AutomationRunLedgerRecord,
) -> Result<ApplicationProblem> {
    if let Some(problem) = post_admission_termination_problem(context, cancellation)? {
        return Ok(problem);
    }
    failure_class_problem(ledger.error_classification)
}

pub(super) fn failure_class_problem(
    classification: Option<AgentTaskFailureClass>,
) -> Result<ApplicationProblem> {
    let diagnostic = SafeDiagnostic::new(
        "application.memory-automation-run.execution-failed",
        "The admitted automatic-memory backend failed before a canonical memory effect committed.",
    )
    .map_err(contract_error)?;
    let problem = match classification {
        Some(AgentTaskFailureClass::Timeout) => {
            ApplicationProblem::timed_out(CancellationStage::EffectInFlight)
        }
        Some(AgentTaskFailureClass::Unavailable) => ApplicationProblem::admitted_unavailable(
            ApplicationUnavailableClassV1::BackendUnavailable,
            diagnostic,
        ),
        Some(AgentTaskFailureClass::Disconnected) => ApplicationProblem::admitted_unavailable(
            ApplicationUnavailableClassV1::BackendDisconnected,
            diagnostic,
        ),
        Some(AgentTaskFailureClass::Retryable) => ApplicationProblem::admitted_unavailable(
            ApplicationUnavailableClassV1::BackendRetryable,
            diagnostic,
        ),
        Some(AgentTaskFailureClass::Denied) => ApplicationProblem::execution_failed(
            ApplicationExecutionFailureClassV1::Denied,
            diagnostic,
        ),
        Some(AgentTaskFailureClass::MalformedOutput) => ApplicationProblem::execution_failed(
            ApplicationExecutionFailureClassV1::MalformedOutput,
            diagnostic,
        ),
        Some(AgentTaskFailureClass::Permanent) | None => ApplicationProblem::execution_failed(
            ApplicationExecutionFailureClassV1::Permanent,
            diagnostic,
        ),
    };
    problem.map_err(contract_error)
}

pub(super) fn runtime_problem(
    context: &RequestContext,
    cancellation: &CancellationSignal,
    error: &TraceDecayError,
) -> Result<ApplicationProblem> {
    if let Some(problem) = post_admission_termination_problem(context, cancellation)? {
        return Ok(problem);
    }
    if error.reset_required_context().is_some() {
        return Ok(ApplicationProblem::ResetRequired {
            diagnostic: SafeDiagnostic::new(
                "application.memory-automation-run.reset-required",
                "The admitted automatic-memory authority requires an explicit reset before reuse.",
            )
            .map_err(contract_error)?,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::Reset],
        });
    }
    ApplicationProblem::execution_failed(
        ApplicationExecutionFailureClassV1::Permanent,
        SafeDiagnostic::new(
            "application.memory-automation-run.execution-failed",
            "The admitted automatic-memory run failed before a canonical memory effect committed.",
        )
        .map_err(contract_error)?,
    )
    .map_err(contract_error)
}

fn post_admission_termination_problem(
    context: &RequestContext,
    cancellation: &CancellationSignal,
) -> Result<Option<ApplicationProblem>> {
    if cancellation.is_cancelled() {
        return ApplicationProblem::cancelled(CancellationStage::EffectInFlight)
            .map(Some)
            .map_err(contract_error);
    }
    match context.admission_at(tracedecay_application::now_micros()) {
        // A request admitted with a cancelled snapshot cannot reach this
        // post-admission mapper, but retain the exact typed state if a corrupt
        // caller violates that boundary.
        RequestAdmission::Cancelled => {
            ApplicationProblem::cancelled(CancellationStage::EffectInFlight)
                .map(Some)
                .map_err(contract_error)
        }
        RequestAdmission::TimedOut => {
            ApplicationProblem::timed_out(CancellationStage::EffectInFlight)
                .map(Some)
                .map_err(contract_error)
        }
        RequestAdmission::Admitted => Ok(None),
    }
}

fn zero_effect_terminal(
    operation: &tracedecay_application::ApplicationOperation,
    context: &RequestContext,
    request: &MemoryAutomationRunRequestV1,
    problem: ApplicationProblem,
) -> Result<AutomationSettledProblem> {
    problem.validate().map_err(contract_error)?;
    let problem = ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        problem,
    )
    .map(|problem| problem.with_owning_layer(ProblemOwningLayer::Application))
    .map_err(contract_error)?;
    MemoryAutomationRunProblemV1::new(
        request.run_id.clone(),
        request.task_kind(),
        context.scope().clone(),
        problem,
        Vec::new(),
        context.request_id(),
    )
    .map_err(contract_error)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_agent_hosts::automation::run_ledger::AutomationRunLedgerRecord;
    use tracedecay_application::{
        ApplicationExecutionFailureClassV1, ApplicationProblemKind, ApplicationUnavailableClassV1,
        CancellationContext, CancellationSignal, CancellationStage, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext, RequestId,
        ResolvedScope,
    };
    use tracedecay_automation::backend::AgentTaskFailureClass;
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::{
        failed_ledger_problem, failure_class_problem, post_admission_termination_problem,
        runtime_problem,
    };

    fn failed_ledger() -> AutomationRunLedgerRecord {
        serde_json::from_value(json!({
            "schema_version": 1,
            "run_id": "run.failed-ledger-problem",
            "trigger": "scheduler",
            "task": "memory_curator",
            "backend": "codex_app_server",
            "status": "failed",
            "accepted_count": 0,
            "rejected_count": 0,
            "error": "backend failed",
            "error_classification": "permanent",
            "started_at": "2026-08-12T00:00:00Z",
            "completed_at": "2026-08-12T00:00:01Z",
            "completed_at_micros": 1786492801000000_i64
        }))
        .expect("failed ledger")
    }

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64)))
            .expect("fixture digest")
    }

    fn context_with_deadline(deadline: UtcMicros) -> RequestContext {
        let scope = ResolvedScope::new(
            ProjectId::new("project.runtime-problem").expect("project"),
            RepositoryId::new("repository.runtime-problem").expect("repository"),
            WorktreeId::new("worktree.runtime-problem").expect("worktree"),
            None,
        )
        .expect("scope");
        RequestContext::new(
            ActorId::new("actor.runtime-problem").expect("actor"),
            scope.clone(),
            CapabilityGrantSnapshot::new(
                CapabilityGrantId::new("grant.runtime-problem").expect("grant"),
                1,
                digest('a'),
                ActorId::new("actor.runtime-problem.issuer").expect("issuer"),
                UtcMicros(1),
                UtcMicros(i64::MAX),
                scope,
                [CapabilityId::new("capability.runtime-problem").expect("capability")]
                    .into_iter()
                    .collect(),
                [UseCaseId::new("use-case.runtime-problem").expect("use case")]
                    .into_iter()
                    .collect(),
                DisclosureClass::Evidence,
            )
            .expect("grant"),
            RequestId::new("request.runtime-problem").expect("request"),
            Deadline::new(deadline).expect("deadline"),
            CancellationContext::active("cancellation.runtime-problem").expect("cancellation"),
        )
        .expect("context")
    }

    fn admitted_context() -> RequestContext {
        context_with_deadline(UtcMicros(i64::MAX))
    }

    #[test]
    fn backend_failure_classes_map_without_parsing_rendered_errors() {
        let cases = [
            (
                AgentTaskFailureClass::Unavailable,
                ApplicationProblemKind::Unavailable,
                Some(ApplicationUnavailableClassV1::BackendUnavailable),
                None,
            ),
            (
                AgentTaskFailureClass::Disconnected,
                ApplicationProblemKind::Unavailable,
                Some(ApplicationUnavailableClassV1::BackendDisconnected),
                None,
            ),
            (
                AgentTaskFailureClass::Retryable,
                ApplicationProblemKind::Unavailable,
                Some(ApplicationUnavailableClassV1::BackendRetryable),
                None,
            ),
            (
                AgentTaskFailureClass::Denied,
                ApplicationProblemKind::ExecutionFailed,
                None,
                Some(ApplicationExecutionFailureClassV1::Denied),
            ),
            (
                AgentTaskFailureClass::MalformedOutput,
                ApplicationProblemKind::ExecutionFailed,
                None,
                Some(ApplicationExecutionFailureClassV1::MalformedOutput),
            ),
            (
                AgentTaskFailureClass::Permanent,
                ApplicationProblemKind::ExecutionFailed,
                None,
                Some(ApplicationExecutionFailureClassV1::Permanent),
            ),
        ];
        for (class, kind, unavailable, execution) in cases {
            let problem = failure_class_problem(Some(class)).expect("typed backend problem");
            assert_eq!(problem.kind(), kind);
            assert_eq!(problem.unavailable_classification(), unavailable);
            assert_eq!(problem.execution_failure_classification(), execution);
        }

        let timeout = failure_class_problem(Some(AgentTaskFailureClass::Timeout))
            .expect("typed timeout problem");
        assert_eq!(timeout.kind(), ApplicationProblemKind::TimedOut);
        assert_eq!(
            timeout.cancellation_stage(),
            Some(CancellationStage::EffectInFlight)
        );
    }

    #[test]
    fn live_post_admission_cancellation_is_not_flattened_to_execution_failure() {
        let context = admitted_context();
        let cancellation =
            CancellationSignal::active("cancellation.runtime-problem").expect("signal");
        assert!(cancellation.cancel(UtcMicros(2)));
        let problem = runtime_problem(
            &context,
            &cancellation,
            &crate::errors::TraceDecayError::Config {
                message: "backend disconnected after admission".to_owned(),
            },
        )
        .expect("typed cancellation");
        assert_eq!(problem.kind(), ApplicationProblemKind::Cancelled);
        assert_eq!(
            problem.cancellation_stage(),
            Some(CancellationStage::EffectInFlight)
        );
        assert_eq!(problem.execution_failure_classification(), None);
    }

    #[test]
    fn elapsed_post_admission_deadline_precedes_backend_failure_classification() {
        let context = context_with_deadline(UtcMicros(2));
        let cancellation =
            CancellationSignal::active("cancellation.runtime-problem").expect("signal");

        let problem = post_admission_termination_problem(&context, &cancellation)
            .expect("typed deadline")
            .expect("elapsed deadline problem");

        assert_eq!(problem.kind(), ApplicationProblemKind::TimedOut);
        assert_eq!(
            problem.cancellation_stage(),
            Some(CancellationStage::EffectInFlight)
        );
        assert_eq!(problem.unavailable_classification(), None);
        assert_eq!(problem.execution_failure_classification(), None);
    }

    #[test]
    fn failed_ledger_live_cancellation_precedes_backend_failure_classification() {
        let context = admitted_context();
        let cancellation =
            CancellationSignal::active("cancellation.runtime-problem").expect("signal");
        assert!(cancellation.cancel(UtcMicros(2)));

        let problem = failed_ledger_problem(&context, &cancellation, &failed_ledger())
            .expect("typed failed-ledger cancellation");

        assert_eq!(problem.kind(), ApplicationProblemKind::Cancelled);
        assert_eq!(
            problem.cancellation_stage(),
            Some(CancellationStage::EffectInFlight)
        );
        assert_eq!(problem.unavailable_classification(), None);
        assert_eq!(problem.execution_failure_classification(), None);
    }

    #[test]
    fn failed_ledger_elapsed_deadline_precedes_backend_failure_classification() {
        let context = context_with_deadline(UtcMicros(2));
        let cancellation =
            CancellationSignal::active("cancellation.runtime-problem").expect("signal");

        let problem = failed_ledger_problem(&context, &cancellation, &failed_ledger())
            .expect("typed failed-ledger timeout");

        assert_eq!(problem.kind(), ApplicationProblemKind::TimedOut);
        assert_eq!(
            problem.cancellation_stage(),
            Some(CancellationStage::EffectInFlight)
        );
        assert_eq!(problem.unavailable_classification(), None);
        assert_eq!(problem.execution_failure_classification(), None);
    }
}
