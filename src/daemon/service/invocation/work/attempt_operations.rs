//! Work attempt lifecycle handler composition.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{
    AdmitWorkSynthesisCommand, ApplicationProblem, CancelWorkAttemptCommand, Deadline,
    RequestContext, RequestId, ResumeWorkAttemptsCommand, RetryWorkAttemptCommandV1,
    SafeDiagnostic, StartWorkAttemptCommand, WorkAttemptStatusRequestV1, WorkSynthesisAttemptV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkAttemptStateV1};
use tracedecay_tool_catalog::UseCaseId;
use tracedecay_usecases::observability::BoundedObservabilityProducerV1;

use crate::daemon_contract::{DaemonInvocationResponse, WorkApplicationOutcomeV1};
use crate::global_db::RegisteredWorkApplicationServicesV1;

use super::super::work_attempt_exec::{WorkAttemptProcessRegistryV1, spawn_attempt_execution};
use super::preparation;
use super::{
    RegisteredWorkRuntime, complete_work_effect, complete_work_read,
    reconcile_active_workflow_fan_out, work_product_problem,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn start_attempt(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_application::WorkProductBindingV1,
    attempt_processes: &Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<&Arc<BoundedObservabilityProducerV1>>,
    project_root: Option<&PathBuf>,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    command: StartWorkAttemptCommand,
) -> DaemonInvocationResponse {
    let started = services
        .run_control()
        .admit_reservation(context, &command.task_id, &command.run_id)
        .and_then(|()| {
            registered
                .database
                .work_product_services(binding.clone())
                .map_err(|_| {
                    work_product_problem(
                        tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                    )
                })
                .and_then(|product| {
                    preparation::current_work_product_revision_pins(registered).and_then(
                        |revisions| {
                            product.attempts().start_against_registered_topology(
                                context,
                                &binding,
                                &revisions,
                                &registered.work_topology_policy,
                                command,
                            )
                        },
                    )
                })
        });
    if let (Ok(attempt), Some(project_root)) = (&started, project_root)
        && attempt.state() == WorkAttemptStateV1::Leased
    {
        spawn_attempt_execution(
            registered.clone(),
            Arc::clone(attempt_processes),
            project_root.clone(),
            attempt.clone(),
            observability_producer.cloned(),
        );
    }
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        started,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::StartAttempt,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn synthesize(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_application::WorkProductBindingV1,
    attempt_processes: &Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<&Arc<BoundedObservabilityProducerV1>>,
    project_root: Option<&PathBuf>,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    command: AdmitWorkSynthesisCommand,
) -> DaemonInvocationResponse {
    let admitted = services
        .run_control()
        .admit_reservation(context, &command.start.task_id, &command.start.run_id)
        .and_then(|()| {
            registered
                .database
                .work_product_services(binding.clone())
                .map_err(|_| {
                    work_product_problem(
                        tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                    )
                })
                .and_then(|product| {
                    preparation::current_work_product_revision_pins(registered).and_then(
                        |revisions| {
                            tracedecay_application::admit_work_synthesis_against_registered_topology(
                                product.synthesis(),
                                context,
                                &binding,
                                &revisions,
                                &registered.work_topology_policy,
                                command,
                            )
                        },
                    )
                })
        });
    if let (Ok(WorkSynthesisAttemptV1::Admitted(admission)), Some(project_root)) =
        (&admitted, project_root)
        && admission.attempt.state() == WorkAttemptStateV1::Leased
    {
        spawn_attempt_execution(
            registered.clone(),
            Arc::clone(attempt_processes),
            project_root.clone(),
            admission.attempt.clone(),
            observability_producer.cloned(),
        );
    }
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        admitted,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::Synthesize,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attempt_status(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    request: WorkAttemptStatusRequestV1,
) -> DaemonInvocationResponse {
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        services.attempts().status(context, &request),
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::AttemptStatus,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cancel_attempt(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    attempt_processes: &Arc<WorkAttemptProcessRegistryV1>,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    command: CancelWorkAttemptCommand,
) -> DaemonInvocationResponse {
    let cancelled = services.attempts().request_cancellation(context, command);
    if let Ok(attempt) = &cancelled {
        attempt_processes.signal_cancellation(&context.scope().worktree_id, attempt.identity());
    }
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        cancelled,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::CancelAttempt,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn retry_attempt(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_application::WorkProductBindingV1,
    attempt_processes: &Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<&Arc<BoundedObservabilityProducerV1>>,
    project_root: Option<&PathBuf>,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    command: RetryWorkAttemptCommandV1,
) -> DaemonInvocationResponse {
    let retried = if project_root.is_none() {
        Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-retry.project-root-unavailable".to_owned(),
            message: "The Work retry runtime owner is unavailable.".to_owned(),
        }))
    } else {
        services
            .run_control()
            .admit_reservation(
                context,
                command.original_attempt.task_id(),
                command.original_attempt.run_id(),
            )
            .and_then(|()| {
                registered
                    .database
                    .work_product_services(binding.clone())
                    .map_err(|_| {
                        work_product_problem(
                            tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                        )
                    })
                    .and_then(|product| {
                        preparation::current_work_product_revision_pins(registered).and_then(
                            |revisions| {
                                product.retry().retry(
                                    context,
                                    &binding,
                                    &revisions,
                                    &registered.work_topology_policy,
                                    command,
                                    observed_at,
                                )
                            },
                        )
                    })
            })
    };
    if let Ok(outcome) = &retried {
        let _ = tracedecay_usecases::observability::record_work_retry_observation(
            observability_producer.map(Arc::as_ref),
            context.scope().project_id.as_str(),
            outcome.receipt(),
        );
        if let (
            tracedecay_application::WorkRetryAttemptOutcomeV1::Created { attempt, .. },
            Some(project_root),
        ) = (outcome, project_root)
        {
            spawn_attempt_execution(
                registered.clone(),
                Arc::clone(attempt_processes),
                project_root.clone(),
                attempt.clone(),
                observability_producer.cloned(),
            );
        }
    }
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        retried,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::RetryAttempt,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_attempts(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    attempt_processes: &Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<&Arc<BoundedObservabilityProducerV1>>,
    project_root: Option<&PathBuf>,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    command: ResumeWorkAttemptsCommand,
) -> DaemonInvocationResponse {
    // Restart recovery is permitted only after this daemon has no live
    // provider holder in the exact worktree. Fencing a process this daemon
    // still owns would strand the durable attempt on a new epoch while the old
    // task can no longer settle it.
    let report = if attempt_processes.holds_worktree(&context.scope().worktree_id) {
        Err(ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "application.work-attempt.live-holder".to_owned(),
                message: "Work attempt recovery requires the current worktree to have no live provider holder."
                    .to_owned(),
            },
            retry: tracedecay_application::RetryDirective::AfterRevalidate,
            legal_actions: vec![tracedecay_application::LegalAction::Refresh],
        })
    } else {
        services.attempts().resume(context, &command)
    };
    if let (Ok(report), Some(project_root)) = (&report, project_root) {
        for attempt in &report.recovery_required {
            spawn_attempt_execution(
                registered.clone(),
                Arc::clone(attempt_processes),
                project_root.clone(),
                attempt.clone(),
                observability_producer.cloned(),
            );
        }
        if let Err(problem) = reconcile_active_workflow_fan_out(
            registered,
            Arc::clone(attempt_processes),
            project_root,
            observability_producer.cloned(),
        ) {
            return DaemonInvocationResponse::problem(request_id, problem);
        }
    }
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        report,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::ResumeAttempts,
    )
}
