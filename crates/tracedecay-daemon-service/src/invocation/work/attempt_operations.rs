//! Work attempt lifecycle handler composition.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::observability::BoundedObservabilityProducerV1;
use tracedecay_contracts::{
    AdmitWorkSynthesisCommand, ApplicationProblem, CancelWorkAttemptCommand, Deadline, LegalAction,
    RequestContext, RequestId, ResumeWorkAttemptsCommand, RetryDirective,
    RetryWorkAttemptCommandV1, SafeDiagnostic, StartWorkAttemptCommand, WorkAttemptStatusRequestV1,
    WorkSynthesisAttemptV1, WorkflowArtifactStorePort,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkAttemptStateV1};
use tracedecay_tool_catalog::UseCaseId;

use tracedecay_application::work::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1,
};
use tracedecay_daemon_protocol::{DaemonInvocationResponse, WorkApplicationOutcomeV1};

use super::super::work_attempt_exec::{WorkAttemptProcessRegistryV1, spawn_attempt_execution};
use super::preparation;
use super::{
    RegisteredWorkRuntime, complete_work_effect, complete_work_read,
    reconcile_active_workflow_fan_out, work_product_problem,
};

fn consume_synthesis_bytes(remaining: &mut u64, bytes: u64) -> Result<(), ApplicationProblem> {
    *remaining = remaining.checked_sub(bytes).ok_or_else(|| ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work-synthesis.source-context-oversized".to_owned(),
            message: "The synthesis instructions and source payloads exceed the admitted protocol byte bound.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    })?;
    Ok(())
}

fn synthesis_source_context(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    command: &AdmitWorkSynthesisCommand,
) -> Result<String, ApplicationProblem> {
    let artifacts = registered.database.workflow_storage().map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-synthesis.artifact-body-unavailable".to_owned(),
            message: "The admitted synthesis source payload authority is unavailable.".to_owned(),
        })
    })?;
    let mut remaining = command
        .start
        .execution_snapshot
        .limits()
        .max_protocol_bytes();
    consume_synthesis_bytes(&mut remaining, command.start.instructions.len() as u64)?;
    consume_synthesis_bytes(&mut remaining, 2)?;
    let mut sources = Vec::with_capacity(command.sources.len());
    for source in &command.sources {
        let attempt = services.attempts().status(
            context,
            &WorkAttemptStatusRequestV1 {
                task_id: source.task_id().clone(),
                run_id: source.run_id().clone(),
                attempt_id: source.attempt_id().clone(),
            },
        )?;
        let mut payloads = Vec::with_capacity(attempt.artifacts().len());
        for artifact in attempt.artifacts() {
            consume_synthesis_bytes(&mut remaining, artifact.byte_length())?;
            let payload = artifacts.load(artifact).map_err(|_| {
                ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.work-synthesis.artifact-body-unavailable".to_owned(),
                    message: "An admitted synthesis source payload is absent or invalid."
                        .to_owned(),
                })
            })?;
            let content = std::str::from_utf8(payload.bytes()).map_err(|_| {
                ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.work-synthesis.artifact-body-not-text".to_owned(),
                    message: "An admitted synthesis source payload is not UTF-8 text.".to_owned(),
                })
            })?;
            payloads.push(serde_json::json!({
                "artifact_id": artifact.artifact_id().as_str(),
                "digest": artifact.digest().as_str(),
                "byte_length": artifact.byte_length(),
                "content": content,
            }));
        }
        sources.push(serde_json::json!({
            "identity": {
                "task_id": source.task_id().as_str(),
                "run_id": source.run_id().as_str(),
                "attempt_id": source.attempt_id().as_str(),
            },
            "state": attempt.state(),
            "terminal": attempt.terminal(),
            "artifacts": payloads,
        }));
    }
    serde_json::to_string(&serde_json::json!({"work_synthesis_sources": sources})).map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-synthesis.source-context-invalid".to_owned(),
            message: "The admitted synthesis source context could not be encoded.".to_owned(),
        })
    })
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.service.work.start_attempt")]
pub(super) fn start_attempt(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_contracts::WorkProductBindingV1,
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
            RegisteredWorkProductServicesV1::attach(&registered.database, binding.clone())
                .map_err(|_| {
                    work_product_problem(
                        tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
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
#[hotpath::measure(label = "daemon.service.work.synthesize")]
pub(super) fn synthesize(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_contracts::WorkProductBindingV1,
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
    mut command: AdmitWorkSynthesisCommand,
) -> DaemonInvocationResponse {
    let admitted = services
        .run_control()
        .admit_reservation(context, &command.start.task_id, &command.start.run_id)
        .and_then(|()| {
            let source_context = synthesis_source_context(registered, services, context, &command)?;
            let prompt_bytes = command
                .start
                .instructions
                .len()
                .saturating_add(2)
                .saturating_add(source_context.len());
            consume_synthesis_bytes(
                &mut command.start.execution_snapshot.limits().max_protocol_bytes(),
                prompt_bytes as u64,
            )?;
            command.start.instructions.push_str("\n\n");
            command.start.instructions.push_str(&source_context);
            RegisteredWorkProductServicesV1::attach(&registered.database, binding.clone())
                .map_err(|_| {
                    work_product_problem(
                        tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                    )
                })
                .and_then(|product| {
                    preparation::current_work_product_revision_pins(registered).and_then(
                        |revisions| {
                            tracedecay_contracts::admit_work_synthesis_against_registered_topology(
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
#[hotpath::measure(label = "daemon.service.work.attempt_status")]
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
#[hotpath::measure(label = "daemon.service.work.cancel_attempt")]
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
#[hotpath::measure(label = "daemon.service.work.retry_attempt")]
pub(super) fn retry_attempt(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    binding: tracedecay_contracts::WorkProductBindingV1,
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
                RegisteredWorkProductServicesV1::attach(&registered.database, binding.clone())
                    .map_err(|_| {
                        work_product_problem(
                            tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
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
        let _ = tracedecay_application::observability::record_work_retry_observation(
            observability_producer.map(Arc::as_ref),
            context.scope().project_id.as_str(),
            outcome.receipt(),
        );
        if let (
            tracedecay_contracts::WorkRetryAttemptOutcomeV1::Created { attempt, .. },
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
        |outcome| WorkApplicationOutcomeV1::RetryAttempt(Box::new(outcome)),
    )
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.service.work.resume_attempts")]
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
            retry: tracedecay_contracts::RetryDirective::AfterRevalidate,
            legal_actions: vec![tracedecay_contracts::LegalAction::Refresh],
        })
    } else {
        services.attempts().resume(context, &command)
    };
    if report.is_ok()
        && let Some(project_root) = project_root
    {
        // Recovery fences the lost provider and reports the retained attempt;
        // it cannot authorize another dispatch under the same identity. A
        // caller that wants another execution must use retry_attempt, whose
        // new attempt identity and effect-safety checks make that choice
        // explicit.
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

#[cfg(test)]
#[test]
fn synthesis_payload_read_budget_is_cumulative_and_refuses_overflow() {
    let mut remaining = 10;
    consume_synthesis_bytes(&mut remaining, 6).unwrap();
    assert!(consume_synthesis_bytes(&mut remaining, 5).is_err());
    assert_eq!(remaining, 4);
    assert!(consume_synthesis_bytes(&mut remaining, u64::MAX).is_err());
    consume_synthesis_bytes(&mut remaining, 4).unwrap();
    assert_eq!(remaining, 0);
}
