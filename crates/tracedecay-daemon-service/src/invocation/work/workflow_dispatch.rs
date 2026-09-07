//! Workflow application dispatch.
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{
    CancellationContext, Deadline, TaskHandoffToken, WorkflowDefinitionLifecycleCommand,
    WorkflowEffectPreparedV1, WorkflowLifecycleOperation, prepare_task_handoff_issue,
    prepare_task_handoff_redeem, prepare_workflow_definition_registration,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};

use tracedecay_daemon_protocol::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkflowApplicationInvocation,
    WorkflowApplicationOutcome,
};

use super::super::work_attempt_exec::WorkAttemptProcessRegistryV1;
use super::workflow_effect_journal::{
    complete_workflow_read, complete_workflow_run_effect, execute_journaled_workflow_effect,
    task_handoff_problem, workflow_effect_problem, workflow_storage_problem,
};
use super::workflow_fan_out::{reconcile_workflow_fan_out, synchronize_fan_out_run_controls};
use super::workflow_run_control::{
    apply_workflow_run_command, cancel_workflow_run, start_workflow_run,
    workflow_coordination_problem, workflow_run_storage_problem,
};
use super::{RegisteredWorkRuntime, work_request_context, workflow_census};

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.service.workflow.execute", future = true)]
pub(crate) async fn execute_workflow_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
    project_root: PathBuf,
    request_id: String,
    request: WorkflowApplicationInvocation,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    worktree_holder_admission: tracedecay_agent_hosts::native_integration::WorktreeHolderAdmissionFenceV1,
) -> DaemonInvocationResponse {
    let Some(holder_root) = project_root.canonicalize().ok() else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    // Fan-out start/resume can durably publish attempts and immediately spawn
    // their processes. Retain one exact-root admission lease through the full
    // workflow command so cleanup cannot observe between those publications.
    let Some(_holder_admission) = worktree_holder_admission.admit_holders([holder_root]).await
    else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    let observed_at = tracedecay_daemon_protocol::invocation_now_micros();
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) =
        tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS
            .iter()
            .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match work_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let input_digest = match canonical_sha256(&request) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let services = match tracedecay_usecases::work::RegisteredWorkflowApplicationServicesV1::attach(
        &registered.database,
    ) {
        Ok(services) => services,
        Err(error) => {
            return DaemonInvocationResponse::problem(request_id, workflow_storage_problem(&error));
        }
    };

    match request {
        WorkflowApplicationInvocation::RegisterDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.register_definition", {
                let prepared =
                    match prepare_workflow_definition_registration(&context, request.definition) {
                        Ok(definition) => WorkflowEffectPreparedV1::register_definition(
                            input_digest.clone(),
                            definition,
                        ),
                        Err(error) => WorkflowEffectPreparedV1::problem(
                            input_digest.clone(),
                            workflow_effect_problem(workflow_coordination_problem(error)),
                        ),
                    };
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::ActivateDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.activate_definition", {
                // Catalog admission rejects before the lifecycle command is
                // journaled; a denial is the same canonical problem effect every
                // other refused mutation records.
                let prepared = match services
                    .definitions()
                    .admit_activation(&request.definition_id, request.definition_version)
                {
                    Ok(()) => WorkflowEffectPreparedV1::activate_definition(
                        input_digest.clone(),
                        WorkflowDefinitionLifecycleCommand {
                            definition_id: request.definition_id,
                            definition_version: request.definition_version,
                            operation: WorkflowLifecycleOperation::Activate,
                            expected_revision: request.expected_revision,
                            transitioned_at: observed_at,
                        },
                    ),
                    Err(error) => WorkflowEffectPreparedV1::problem(
                        input_digest.clone(),
                        workflow_effect_problem(workflow_coordination_problem(error)),
                    ),
                };
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::RetireDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.retire_definition", {
                let prepared = WorkflowEffectPreparedV1::retire_definition(
                    input_digest.clone(),
                    WorkflowDefinitionLifecycleCommand {
                        definition_id: request.definition_id,
                        definition_version: request.definition_version,
                        operation: WorkflowLifecycleOperation::Retire,
                        expected_revision: request.expected_revision,
                        transitioned_at: observed_at,
                    },
                );
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::RejectDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.reject_definition", {
                let prepared = WorkflowEffectPreparedV1::reject_definition(
                    input_digest.clone(),
                    WorkflowDefinitionLifecycleCommand {
                        definition_id: request.definition_id,
                        definition_version: request.definition_version,
                        operation: WorkflowLifecycleOperation::Reject,
                        expected_revision: request.expected_revision,
                        transitioned_at: observed_at,
                    },
                );
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::ValidateDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.validate_definition", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    services
                        .definitions()
                        .validate(request.definition)
                        .map_err(workflow_coordination_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::ValidateDefinition,
                )
            })
        }
        WorkflowApplicationInvocation::GetDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.get_definition", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    services
                        .definitions()
                        .get(&request.definition_id, request.definition_version)
                        .map_err(workflow_coordination_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::GetDefinition,
                )
            })
        }
        WorkflowApplicationInvocation::ListDefinitions(_) => {
            hotpath::measure_block!("daemon.service.workflow.list_definitions", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    services
                        .definitions()
                        .list()
                        .map_err(workflow_coordination_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::ListDefinitions,
                )
            })
        }
        WorkflowApplicationInvocation::DefinitionHistory(request) => {
            hotpath::measure_block!("daemon.service.workflow.definition_history", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    services
                        .definitions()
                        .history(&request.definition_id)
                        .map_err(workflow_coordination_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::DefinitionHistory,
                )
            })
        }
        WorkflowApplicationInvocation::DiffDefinition(request) => {
            hotpath::measure_block!("daemon.service.workflow.diff_definition", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    services
                        .definitions()
                        .diff(
                            &request.definition_id,
                            request.from_version,
                            request.to_version,
                        )
                        .map_err(workflow_coordination_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::DiffDefinition,
                )
            })
        }
        WorkflowApplicationInvocation::HandoffIssue(request) => {
            hotpath::measure_block!("daemon.service.workflow.handoff_issue", {
                let prepared = match TaskHandoffToken::new(request.secret).and_then(|token| {
                    prepare_task_handoff_issue(
                        &context,
                        request.scope,
                        &token,
                        observed_at,
                        request.frontier,
                    )
                }) {
                    Ok(grant) => {
                        WorkflowEffectPreparedV1::handoff_issue(input_digest.clone(), grant)
                    }
                    Err(error) => WorkflowEffectPreparedV1::problem(
                        input_digest.clone(),
                        workflow_effect_problem(task_handoff_problem(error)),
                    ),
                };
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::HandoffRedeem(request) => {
            hotpath::measure_block!("daemon.service.workflow.handoff_redeem", {
                let scope = request.expected_scope;
                let prepared = match TaskHandoffToken::new(request.secret)
                    .and_then(|token| prepare_task_handoff_redeem(&context, &token, &scope))
                {
                    Ok(token_digest) => WorkflowEffectPreparedV1::handoff_redeem(
                        input_digest.clone(),
                        token_digest,
                        scope,
                        observed_at,
                    ),
                    Err(error) => WorkflowEffectPreparedV1::problem(
                        input_digest.clone(),
                        workflow_effect_problem(task_handoff_problem(error)),
                    ),
                };
                execute_journaled_workflow_effect(
                    &registered,
                    services.effects(),
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    prepared,
                    observed_at,
                    deadline,
                )
            })
        }
        WorkflowApplicationInvocation::StartRun(request) => {
            hotpath::measure_block!("daemon.service.workflow.start_run", {
                let result = start_workflow_run(
                    &registered,
                    &services,
                    &context,
                    *request,
                    &input_digest,
                    observed_at,
                    Arc::clone(&attempt_processes),
                    &project_root,
                    observability_producer.clone(),
                );
                complete_workflow_run_effect(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    result,
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::StartRun,
                )
            })
        }
        WorkflowApplicationInvocation::PauseRun(request) => {
            hotpath::measure_block!("daemon.service.workflow.pause_run", {
                let result = apply_workflow_run_command(
                    &services,
                    &request.run_id,
                    request.expected_sequence,
                    tracedecay_domain::WorkflowRunCommand::Pause,
                    request.command_id,
                    &input_digest,
                    observed_at,
                )
                .and_then(|projection| {
                    synchronize_fan_out_run_controls(
                        &registered,
                        &context,
                        &projection,
                        true,
                        observed_at,
                    )?;
                    workflow_census::persist_workflow_fan_out_census(
                        &registered,
                        &services,
                        &context,
                        &projection,
                        observed_at,
                        observability_producer.clone(),
                    );
                    Ok(projection)
                });
                complete_workflow_run_effect(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    result,
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::PauseRun,
                )
            })
        }
        WorkflowApplicationInvocation::ResumeRun(request) => {
            hotpath::measure_block!("daemon.service.workflow.resume_run", {
                let result = apply_workflow_run_command(
                    &services,
                    &request.run_id,
                    request.expected_sequence,
                    tracedecay_domain::WorkflowRunCommand::Resume,
                    request.command_id,
                    &input_digest,
                    observed_at,
                )
                .and_then(|projection| {
                    synchronize_fan_out_run_controls(
                        &registered,
                        &context,
                        &projection,
                        false,
                        observed_at,
                    )?;
                    reconcile_workflow_fan_out(
                        &registered,
                        &services,
                        &context,
                        projection,
                        observed_at,
                        Arc::clone(&attempt_processes),
                        &project_root,
                        observability_producer.clone(),
                    )
                });
                complete_workflow_run_effect(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    result,
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::ResumeRun,
                )
            })
        }
        WorkflowApplicationInvocation::CancelRun(request) => {
            hotpath::measure_block!("daemon.service.workflow.cancel_run", {
                let result = cancel_workflow_run(
                    &registered,
                    &services,
                    &context,
                    request,
                    &input_digest,
                    observed_at,
                    Arc::clone(&attempt_processes),
                    &project_root,
                    observability_producer.clone(),
                );
                complete_workflow_run_effect(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    result,
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::CancelRun,
                )
            })
        }
        WorkflowApplicationInvocation::GetRun(request) => {
            hotpath::measure_block!("daemon.service.workflow.get_run", {
                complete_workflow_read(
                    &registered,
                    request_id,
                    &context,
                    canonical_request_id,
                    operation_key,
                    use_case,
                    input_digest,
                    tracedecay_application::WorkflowRunStoragePort::projection(
                        services.effects(),
                        &request.run_id,
                    )
                    .map_err(workflow_run_storage_problem),
                    observed_at,
                    deadline,
                    WorkflowApplicationOutcome::GetRun,
                )
            })
        }
    }
}
