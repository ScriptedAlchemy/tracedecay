use std::path::Path;
use std::sync::Arc;

use tracedecay_application::{ApplicationProblem, RequestContext};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::daemon_contract::DaemonInvocationProblem;

use super::super::current_micros;
use super::workflow_run_control::workflow_run_problem;
use super::{RegisteredWorkRuntime, work_background_context};

mod recovery;

pub(in crate::daemon::service::invocation) use recovery::{
    WorkflowFanOutRecoveryOwnerV1, reconcile_active_workflow_fan_out,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_workflow_fan_out(
    registered: &RegisteredWorkRuntime,
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    mut projection: tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    attempt_processes: Arc<super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let initial_sequence = projection.sequence();
    if projection.status() == tracedecay_domain::WorkflowRunStatus::Cancelling {
        let (projection, cancelled_children) = reconcile_cancelled_fan_out(
            registered,
            services,
            context,
            projection,
            observed_at,
            &attempt_processes,
        )?;
        super::workflow_census::persist_workflow_fan_out_census(
            registered,
            services,
            context,
            &projection,
            observed_at,
            observability_producer.clone(),
        );
        if cancelled_children || projection.sequence() != initial_sequence {
            super::publish_committed_task_activity_in_background(
                registered.database.clone(),
                project_root.to_path_buf(),
                None,
            );
        }
        return Ok(projection);
    }
    if projection.status() != tracedecay_domain::WorkflowRunStatus::Running {
        super::workflow_census::persist_workflow_fan_out_census(
            registered,
            services,
            context,
            &projection,
            observed_at,
            observability_producer.clone(),
        );
        return Ok(projection);
    }
    let work = registered
        .database
        .work_application_services()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let authority = tracedecay_domain::WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    let plans = projection
        .fan_out_plans()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut cancelled_children = false;
    for plan in plans {
        if plan.authority != authority {
            return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
        }
        if projection
            .step(&plan.step_id)
            .is_some_and(|step| step.status() == tracedecay_domain::WorkflowStepStatus::Ready)
        {
            let placement = tracedecay_domain::WorkflowPlacementReceipt::new(
                projection.run_id().clone(),
                plan.step_id.clone(),
                plan.execution_snapshot.route().clone(),
                plan.execution_snapshot.backend(),
                plan.execution_snapshot.model().to_owned(),
                projection
                    .definition()
                    .pinned_configuration_digest()
                    .clone(),
                projection.pinned_topology_digest().clone(),
                projection.pinned_provider_registry_digest().clone(),
                registered.work_topology_policy.placement.clone(),
            )
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            projection = apply_scheduler_command(
                services,
                &projection,
                tracedecay_domain::WorkflowRunCommand::StartStep {
                    step_id: plan.step_id.clone(),
                    placement,
                },
                "start",
                &plan.plan_digest,
                observed_at,
            )?;
        }

        let mut active = 0usize;
        let mut terminal = Vec::new();
        let mut recovery_required = Vec::new();
        let mut known = std::collections::BTreeSet::new();
        for identity in projection.released_fan_out_attempts() {
            if !plan
                .children
                .iter()
                .any(|child| &child.attempt_identity == identity)
            {
                continue;
            }
            match work.attempts().status(
                context,
                &tracedecay_application::WorkAttemptStatusRequestV1 {
                    task_id: identity.task_id().clone(),
                    run_id: identity.run_id().clone(),
                    attempt_id: identity.attempt_id().clone(),
                },
            ) {
                Ok(attempt) if attempt.is_terminal() => {
                    known.insert(identity.clone());
                    terminal.push(attempt);
                }
                Ok(attempt)
                    if attempt.state()
                        == tracedecay_domain::WorkAttemptStateV1::RecoveryRequired =>
                {
                    known.insert(identity.clone());
                    active += 1;
                    recovery_required.push(attempt);
                }
                Ok(_) => {
                    known.insert(identity.clone());
                    active += 1;
                }
                Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => active += 1,
                Err(_) => return Err(DaemonInvocationProblem::Unavailable),
            }
        }
        let newly_settled = terminal
            .iter()
            .map(|attempt| attempt.identity().clone())
            .filter(|identity| !projection.settled_fan_out_attempts().contains(identity))
            .collect::<Vec<_>>();
        if !newly_settled.is_empty() {
            projection = apply_scheduler_command(
                services,
                &projection,
                tracedecay_domain::WorkflowRunCommand::SettleFanOutChildren {
                    step_id: plan.step_id.clone(),
                    attempts: newly_settled,
                },
                "observe-terminal",
                &plan.plan_digest,
                observed_at,
            )?;
        }
        for attempt in recovery_required {
            super::super::work_attempt_exec::spawn_attempt_execution(
                registered.clone(),
                Arc::clone(&attempt_processes),
                project_root.to_path_buf(),
                attempt,
                observability_producer.clone(),
            );
        }
        let failed_fast = plan.failure_policy
            == tracedecay_domain::WorkflowFanOutFailurePolicyV1::FailFast
            && terminal
                .iter()
                .any(|attempt| attempt.state() != tracedecay_domain::WorkAttemptStateV1::Succeeded);
        if failed_fast {
            cancelled_children |= request_fan_out_cancellation(
                context,
                &work,
                &attempt_processes,
                &plan,
                &terminal,
                observed_at,
            )?;
            let released = plan
                .children
                .iter()
                .filter(|child| {
                    projection
                        .released_fan_out_attempts()
                        .contains(&child.attempt_identity)
                })
                .count();
            if terminal.len() == released {
                projection =
                    settle_workflow_fan_out(services, &projection, &plan, &terminal, observed_at)?;
            }
            continue;
        }
        let capacity = usize::from(plan.maximum_parallel.get()).saturating_sub(active);
        let release = plan
            .children
            .iter()
            .filter(|child| {
                !projection
                    .released_fan_out_attempts()
                    .contains(&child.attempt_identity)
            })
            .take(capacity)
            .map(|child| child.attempt_identity.clone())
            .collect::<Vec<_>>();
        if !release.is_empty() {
            projection = apply_scheduler_command(
                services,
                &projection,
                tracedecay_domain::WorkflowRunCommand::ReleaseFanOutChildren {
                    step_id: plan.step_id.clone(),
                    attempts: release,
                },
                "release",
                &plan.plan_digest,
                observed_at,
            )?;
        }
        for child in &plan.children {
            if !projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
                || known.contains(&child.attempt_identity)
            {
                continue;
            }
            admit_workflow_child(registered, context, &work, child, observed_at)?;
            let product_binding = workflow_product_binding()?;
            let product = registered
                .database
                .work_product_services(product_binding.clone())
                .map_err(|_| DaemonInvocationProblem::Unavailable)?;
            let revisions = workflow_product_revision_pins(registered)?;
            let attempt = product
                .attempts()
                .start_against_registered_topology(
                    context,
                    &product_binding,
                    &revisions,
                    &registered.work_topology_policy,
                    tracedecay_application::StartWorkAttemptCommand {
                        task_id: child.task_id.clone(),
                        run_id: child.attempt_identity.run_id().clone(),
                        attempt_id: child.attempt_identity.attempt_id().clone(),
                        operation: plan.operation.clone(),
                        execution_snapshot: plan.execution_snapshot.clone(),
                        worktree_root: project_root
                            .to_str()
                            .ok_or(DaemonInvocationProblem::InvalidRequest)?
                            .to_owned(),
                        reference: plan.reference.clone(),
                        commit: plan.commit.clone(),
                        instructions: child.instructions.clone(),
                        effect_state: plan.effect_state,
                        occurred_at: observed_at,
                    },
                )
                .map_err(|_| DaemonInvocationProblem::Unavailable)?;
            if attempt.state() == tracedecay_domain::WorkAttemptStateV1::Leased {
                super::super::work_attempt_exec::spawn_attempt_execution(
                    registered.clone(),
                    Arc::clone(&attempt_processes),
                    project_root.to_path_buf(),
                    attempt,
                    observability_producer.clone(),
                );
            }
        }
        if terminal.len() == plan.children.len() {
            projection =
                settle_workflow_fan_out(services, &projection, &plan, &terminal, observed_at)?;
        }
    }
    super::workflow_census::persist_workflow_fan_out_census(
        registered,
        services,
        context,
        &projection,
        observed_at,
        observability_producer,
    );
    if cancelled_children || projection.sequence() != initial_sequence {
        super::publish_committed_task_activity_in_background(
            registered.database.clone(),
            project_root.to_path_buf(),
            None,
        );
    }
    Ok(projection)
}

fn reconcile_cancelled_fan_out(
    registered: &RegisteredWorkRuntime,
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    projection: tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
) -> Result<(tracedecay_domain::WorkflowRunProjection, bool), DaemonInvocationProblem> {
    let work = registered
        .database
        .work_application_services()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let mut all_terminal = true;
    let mut cancelled_children = false;
    for plan in projection.fan_out_plans().values() {
        cancelled_children |= request_fan_out_cancellation(
            context,
            &work,
            attempt_processes,
            plan,
            &[],
            observed_at,
        )?;
        for child in &plan.children {
            if !projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
            {
                continue;
            }
            match work.attempts().status(
                context,
                &tracedecay_application::WorkAttemptStatusRequestV1 {
                    task_id: child.task_id.clone(),
                    run_id: child.attempt_identity.run_id().clone(),
                    attempt_id: child.attempt_identity.attempt_id().clone(),
                },
            ) {
                Ok(attempt) => all_terminal &= attempt.is_terminal(),
                Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => {}
                Err(_) => return Err(DaemonInvocationProblem::Unavailable),
            }
        }
    }
    if !all_terminal {
        return Ok((projection, cancelled_children));
    }
    let plan_digest = projection
        .fan_out_plans()
        .values()
        .next()
        .map(|plan| &plan.plan_digest)
        .ok_or(DaemonInvocationProblem::InvalidRequest)?;
    let projection = apply_scheduler_command(
        services,
        &projection,
        tracedecay_domain::WorkflowRunCommand::ReconcileCancelled,
        "cancel",
        plan_digest,
        observed_at,
    )?;
    Ok((projection, cancelled_children))
}

fn settle_workflow_fan_out(
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    projection: &tracedecay_domain::WorkflowRunProjection,
    plan: &tracedecay_domain::WorkflowFanOutPlanV1,
    attempts: &[tracedecay_domain::WorkAttemptV1],
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let succeeded = attempts
        .iter()
        .filter(|attempt| attempt.state() == tracedecay_domain::WorkAttemptStateV1::Succeeded)
        .collect::<Vec<_>>();
    let required = match plan.failure_policy {
        tracedecay_domain::WorkflowFanOutFailurePolicyV1::FailFast => attempts.len(),
        tracedecay_domain::WorkflowFanOutFailurePolicyV1::Collect => 1,
        tracedecay_domain::WorkflowFanOutFailurePolicyV1::RequireAtLeast { successes } => {
            usize::from(successes.get())
        }
    };
    let definition_step = projection
        .definition()
        .steps()
        .iter()
        .find(|step| step.step_id == plan.step_id)
        .ok_or(DaemonInvocationProblem::InvalidRequest)?;
    let outputs = if succeeded.len() >= required {
        definition_step
            .outputs
            .iter()
            .enumerate()
            .map(|(output_index, output_name)| {
                let artifacts = succeeded
                    .iter()
                    .map(|attempt| {
                        attempt
                            .artifacts()
                            .get(output_index)
                            .cloned()
                            .map(|artifact| {
                                tracedecay_domain::WorkflowOutputArtifact::new(
                                    attempt.identity().clone(),
                                    artifact,
                                )
                            })
                            .ok_or(DaemonInvocationProblem::Unavailable)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                tracedecay_domain::WorkflowStepOutput::new(output_name.clone(), artifacts)
                    .map_err(|_| DaemonInvocationProblem::Unavailable)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let step = projection
        .step(&plan.step_id)
        .ok_or(DaemonInvocationProblem::InvalidRequest)?;
    let placement_digest = step
        .placement_receipt()
        .map(|receipt| receipt.placement_digest().clone())
        .ok_or(DaemonInvocationProblem::Unavailable)?;
    let effect_digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-fan-out-terminal.v1",
        &plan.plan_digest,
        attempts,
        &outputs,
    ))
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let completed = succeeded.len() >= required;
    let outcome = if completed {
        tracedecay_domain::WorkflowStepEffectOutcome::Completed
    } else {
        tracedecay_domain::WorkflowStepEffectOutcome::Failed
    };
    let receipt = tracedecay_domain::WorkflowStepEffectReceipt::new(
        projection.run_id().clone(),
        plan.step_id.clone(),
        placement_digest,
        outcome,
        effect_digest,
        &outputs,
    )
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let command = if completed {
        tracedecay_domain::WorkflowRunCommand::CompleteStep {
            step_id: plan.step_id.clone(),
            outputs,
            effect_receipt: receipt,
        }
    } else {
        tracedecay_domain::WorkflowRunCommand::FailStep {
            step_id: plan.step_id.clone(),
            outputs: Vec::new(),
            effect_receipt: receipt,
        }
    };
    apply_scheduler_command(
        services,
        projection,
        command,
        "settle",
        &plan.plan_digest,
        observed_at,
    )
}

fn request_fan_out_cancellation(
    context: &RequestContext,
    services: &crate::global_db::RegisteredWorkApplicationServicesV1,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
    plan: &tracedecay_domain::WorkflowFanOutPlanV1,
    terminal: &[tracedecay_domain::WorkAttemptV1],
    occurred_at: UtcMicros,
) -> Result<bool, DaemonInvocationProblem> {
    let mut cancelled_children = false;
    for child in &plan.children {
        if terminal
            .iter()
            .any(|attempt| attempt.identity() == &child.attempt_identity)
        {
            continue;
        }
        let Ok(attempt) = services.attempts().status(
            context,
            &tracedecay_application::WorkAttemptStatusRequestV1 {
                task_id: child.task_id.clone(),
                run_id: child.attempt_identity.run_id().clone(),
                attempt_id: child.attempt_identity.attempt_id().clone(),
            },
        ) else {
            continue;
        };
        if !matches!(
            attempt.state(),
            tracedecay_domain::WorkAttemptStateV1::Leased
                | tracedecay_domain::WorkAttemptStateV1::Running
                | tracedecay_domain::WorkAttemptStateV1::RecoveryRequired
        ) {
            continue;
        }
        let request_id = tracedecay_domain::WorkCancellationRequestId::new(format!(
            "workflow-fail-fast:{}:{}",
            plan.plan_digest.as_str(),
            child.attempt_identity.attempt_id().as_str()
        ))
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        let cancelled = services
            .attempts()
            .request_cancellation(
                context,
                tracedecay_application::CancelWorkAttemptCommand {
                    task_id: child.task_id.clone(),
                    run_id: child.attempt_identity.run_id().clone(),
                    attempt_id: child.attempt_identity.attempt_id().clone(),
                    request_id,
                    occurred_at,
                },
            )
            .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        attempt_processes.signal_cancellation(&context.scope().worktree_id, cancelled.identity());
        cancelled_children = true;
    }
    Ok(cancelled_children)
}

fn apply_scheduler_command(
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    projection: &tracedecay_domain::WorkflowRunProjection,
    command: tracedecay_domain::WorkflowRunCommand,
    operation: &str,
    plan_digest: &ManifestDigest,
    occurred_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-scheduler-command.v1",
        projection.run_id(),
        projection.sequence(),
        operation,
        plan_digest,
        &command,
    ))
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let command_id = tracedecay_domain::WorkCommandId::new(format!(
        "workflow-scheduler-{operation}:{}",
        digest.as_str()
    ))
    .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    tracedecay_application::WorkflowRunService::new(services.effects().clone())
        .apply(
            projection.run_id(),
            projection.sequence(),
            command,
            tracedecay_domain::WorkflowRunEventContext {
                command_id,
                input_digest: digest,
                occurred_at,
            },
        )
        .map_err(workflow_run_problem)
}

pub(in crate::daemon::service::invocation) fn admit_workflow_child(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    services: &crate::global_db::RegisteredWorkApplicationServicesV1,
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    occurred_at: UtcMicros,
) -> Result<(), DaemonInvocationProblem> {
    let selection = tracedecay_application::WorkProductSelectionScopeV1::relations(
        [tracedecay_application::WorkRelationScopeV1::Repository {
            project_id: context.scope().project_id.clone(),
            repository_id: context.scope().repository_id.clone(),
        }]
        .into_iter()
        .collect(),
    )
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let binding = workflow_product_binding()?;
    let product = registered
        .database
        .work_product_services(binding.clone())
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let graph = current_workflow_product_graph(&product, context, selection.clone(), occurred_at)?;
    match graph {
        None => {
            let initial_graph = tracedecay_domain::WorkProductGraphV1::new(
                tracedecay_domain::WorkGraphVersionV1::initial(),
                vec![child.initiative.clone()],
                vec![child.plan.clone()],
                vec![child.milestone.clone()],
                vec![child.item.clone()],
            )
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
            let revisions = workflow_product_revision_pins(registered)?;
            product
                .mutations()
                .create(
                    context,
                    &binding,
                    tracedecay_application::CreateWorkProductRequestV1 {
                        selection: selection.clone(),
                        initial_graph,
                        mutation: tracedecay_application::WorkProductMutationIdentityV1 {
                            expected_authority:
                                tracedecay_application::WorkProductExpectedAuthorityV1::NoPriorGraph,
                            command_id: child.create_command_id.clone(),
                            causation_event_id: None,
                            evidence: Vec::new(),
                            occurred_at,
                            revisions,
                        },
                    },
                )
                .map_err(|_| DaemonInvocationProblem::Unavailable)?;
        }
        Some(graph) if graph.item(&child.task_id).is_none() => {
            apply_workflow_child_product_mutation(
                registered,
                &product,
                context,
                &binding,
                selection.clone(),
                tracedecay_application::WorkProductChangeDraftV1::CreateTask {
                    initiative: child.initiative.clone(),
                    plan: child.plan.clone(),
                    milestone: child.milestone.clone(),
                    item: Box::new(child.item.clone()),
                },
                child.create_command_id.clone(),
                occurred_at,
            )?;
        }
        Some(graph) if !workflow_child_task_matches(&graph, child) => {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        Some(_) => {}
    }

    let graph = current_workflow_product_graph(&product, context, selection.clone(), occurred_at)?
        .ok_or(DaemonInvocationProblem::Unavailable)?;
    let item = graph
        .item(&child.task_id)
        .ok_or(DaemonInvocationProblem::Unavailable)?;
    match item.accepted_proposal() {
        None => apply_workflow_child_product_mutation(
            registered,
            &product,
            context,
            &binding,
            selection.clone(),
            tracedecay_application::WorkProductChangeDraftV1::DecideProposal {
                proposal: child.proposal.clone(),
                disposition: tracedecay_domain::WorkProposalDispositionV1::Accepted,
            },
            child.proposal_command_id.clone(),
            occurred_at,
        )?,
        Some(proposal_id)
            if proposal_id == child.proposal.proposal_id()
                && graph.proposal_decisions().iter().any(|decision| {
                    decision.proposal() == &child.proposal
                        && decision.disposition()
                            == &tracedecay_domain::WorkProposalDispositionV1::Accepted
                }) => {}
        Some(_) => return Err(DaemonInvocationProblem::InvalidRequest),
    }

    let graph = current_workflow_product_graph(&product, context, selection.clone(), occurred_at)?
        .ok_or(DaemonInvocationProblem::Unavailable)?;
    let item = graph
        .item(&child.task_id)
        .ok_or(DaemonInvocationProblem::Unavailable)?;
    if !item.is_execution_admitted() {
        apply_workflow_child_product_mutation(
            registered,
            &product,
            context,
            &binding,
            selection,
            tracedecay_application::WorkProductChangeDraftV1::AdmitExecution {
                task_id: child.task_id.clone(),
            },
            child.admit_command_id.clone(),
            occurred_at,
        )?;
    }
    services
        .run_control()
        .admit_reservation(context, &child.task_id, child.attempt_identity.run_id())
        .map_err(|_| DaemonInvocationProblem::Unavailable)
}

pub(in crate::daemon::service::invocation) fn workflow_product_binding()
-> Result<tracedecay_application::WorkProductBindingV1, DaemonInvocationProblem> {
    Ok(tracedecay_application::WorkProductBindingV1::new(
        CapabilityId::new("capability.work.mutate_graph")
            .map_err(|_| DaemonInvocationProblem::Unavailable)?,
        UseCaseId::new("use-case.work.mutate_graph")
            .map_err(|_| DaemonInvocationProblem::Unavailable)?,
    ))
}

pub(in crate::daemon::service::invocation) fn workflow_product_revision_pins(
    registered: &RegisteredWorkRuntime,
) -> Result<tracedecay_application::WorkProductRevisionPinsV1, DaemonInvocationProblem> {
    super::preparation::current_work_product_revision_pins(registered)
        .map_err(|_| DaemonInvocationProblem::Unavailable)
}

fn current_workflow_product_graph(
    product: &tracedecay_global_db::RegisteredWorkProductServicesV1,
    context: &RequestContext,
    selection: tracedecay_application::WorkProductSelectionScopeV1,
    observed_at: UtcMicros,
) -> Result<Option<tracedecay_domain::WorkProductGraphV1>, DaemonInvocationProblem> {
    match product.reads().read_graph(
        context,
        tracedecay_application::WorkGraphReadRequestV1::current(selection, observed_at),
    ) {
        Ok(tracedecay_application::WorkGraphReadV1::Current { snapshot, .. }) => {
            Ok(Some(snapshot.graph().clone()))
        }
        Ok(_) => Err(DaemonInvocationProblem::Unavailable),
        Err(tracedecay_application::WorkProductApplicationErrorV1::NotFoundOrNotAuthorized) => {
            Ok(None)
        }
        Err(_) => Err(DaemonInvocationProblem::Unavailable),
    }
}

fn workflow_child_task_matches(
    graph: &tracedecay_domain::WorkProductGraphV1,
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
) -> bool {
    graph.item(&child.task_id) == Some(&child.item)
        && graph
            .initiatives()
            .iter()
            .any(|initiative| initiative == &child.initiative)
        && graph.plans().iter().any(|plan| plan == &child.plan)
        && graph
            .milestones()
            .iter()
            .any(|milestone| milestone == &child.milestone)
}

#[allow(clippy::too_many_arguments)]
fn apply_workflow_child_product_mutation(
    registered: &RegisteredWorkRuntime,
    product: &tracedecay_global_db::RegisteredWorkProductServicesV1,
    context: &RequestContext,
    binding: &tracedecay_application::WorkProductBindingV1,
    selection: tracedecay_application::WorkProductSelectionScopeV1,
    change: tracedecay_application::WorkProductChangeDraftV1,
    command_id: tracedecay_domain::WorkCommandId,
    occurred_at: UtcMicros,
) -> Result<(), DaemonInvocationProblem> {
    let revisions = workflow_product_revision_pins(registered)?;
    let mutation = product
        .mutations()
        .prepare_mutation(
            context,
            binding,
            tracedecay_application::PrepareWorkProductMutationRequestV1 {
                selection,
                change,
                causation_event_id: None,
                evidence: Vec::new(),
            },
            command_id,
            occurred_at,
            revisions.clone(),
        )
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    product
        .mutations()
        .mutate(context, binding, mutation, &revisions)
        .map(|_| ())
        .map_err(|_| DaemonInvocationProblem::Unavailable)
}

pub(in crate::daemon::service::invocation) fn reconcile_workflow_fan_out_after_attempt(
    registered: &RegisteredWorkRuntime,
    attempt_processes: Arc<super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
) {
    let Ok(services) = registered.database.workflow_application_services() else {
        return;
    };
    let Ok(projection) = tracedecay_application::WorkflowRunStoragePort::projection(
        services.effects(),
        identity.run_id(),
    ) else {
        return;
    };
    let Ok(context) = work_background_context(registered, identity) else {
        return;
    };
    if let Err(problem) = reconcile_workflow_fan_out(
        registered,
        &services,
        &context,
        projection,
        current_micros(),
        attempt_processes,
        project_root,
        observability_producer,
    ) {
        tracing::warn!(?problem, "workflow fan-out reconciliation did not complete");
    }
}

pub(super) fn synchronize_fan_out_run_controls(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    projection: &tracedecay_domain::WorkflowRunProjection,
    paused: bool,
    occurred_at: UtcMicros,
) -> Result<(), DaemonInvocationProblem> {
    let work = registered
        .database
        .work_application_services()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
    for plan in projection.fan_out_plans().values() {
        for child in &plan.children {
            if !projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
            {
                continue;
            }
            let reading = match work.run_control().read(
                context,
                &tracedecay_application::WorkRunControlRequestV1 {
                    task_id: child.task_id.clone(),
                    run_id: child.attempt_identity.run_id().clone(),
                },
            ) {
                Ok(reading) => reading,
                Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => continue,
                Err(_) => return Err(DaemonInvocationProblem::Unavailable),
            };
            match (paused, reading) {
                (true, tracedecay_application::WorkRunControlReadingV1::Uncontrolled { .. }) => {
                    work.run_control()
                        .pause(
                            context,
                            tracedecay_application::PauseWorkRunCommand {
                                task_id: child.task_id.clone(),
                                run_id: child.attempt_identity.run_id().clone(),
                                reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                                expected_authority_version: None,
                                occurred_at,
                            },
                        )
                        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
                }
                (
                    true,
                    tracedecay_application::WorkRunControlReadingV1::Controlled { control, .. },
                ) if control.state() == tracedecay_domain::WorkRunControlStateV1::Running => {
                    work.run_control()
                        .pause(
                            context,
                            tracedecay_application::PauseWorkRunCommand {
                                task_id: child.task_id.clone(),
                                run_id: child.attempt_identity.run_id().clone(),
                                reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                                expected_authority_version: Some(control.authority().get()),
                                occurred_at,
                            },
                        )
                        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
                }
                (
                    false,
                    tracedecay_application::WorkRunControlReadingV1::Controlled { control, .. },
                ) if control.state() == tracedecay_domain::WorkRunControlStateV1::Paused => {
                    work.run_control()
                        .resume(
                            context,
                            tracedecay_application::ResumeWorkRunCommand {
                                task_id: child.task_id.clone(),
                                run_id: child.attempt_identity.run_id().clone(),
                                reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                                expected_authority_version: control.authority().get(),
                                occurred_at,
                            },
                        )
                        .map_err(|_| DaemonInvocationProblem::Unavailable)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
