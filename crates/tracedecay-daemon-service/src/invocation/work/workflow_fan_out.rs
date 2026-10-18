use std::path::Path;
use std::sync::Arc;

use tracedecay_application::work::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1,
    RegisteredWorkflowApplicationServicesV1,
};
use tracedecay_contracts::{ApplicationProblem, RequestContext, WorkflowRunReadPort};
use tracedecay_domain::{ManifestDigest, UtcMicros, WorkAttemptIdentityV1, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::workflow_run_control::{
    workflow_invalid_request, workflow_not_found, workflow_run_problem,
    workflow_runtime_unavailable,
};
use super::{RegisteredWorkRuntime, work_background_context};
use tracedecay_contracts::now_micros;

mod recovery;

pub(crate) use recovery::{WorkflowFanOutRecoveryOwnerV1, reconcile_active_workflow_fan_out};

#[allow(clippy::too_many_arguments)]
pub(super) async fn reconcile_workflow_fan_out(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    mut projection: tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    attempt_processes: Arc<super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    observability_producer: Option<
        Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    >,
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
    let initial_sequence = projection.sequence();
    if projection.status() == tracedecay_domain::WorkflowRunStatus::Cancelling {
        let (projection, cancelled_children) = reconcile_cancelled_fan_out(
            registered,
            services,
            context,
            projection,
            observed_at,
            &attempt_processes,
        )
        .await?;
        super::workflow_census::persist_workflow_fan_out_census(
            registered,
            services,
            context,
            &projection,
            observed_at,
            observability_producer.clone(),
        )
        .await;
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
        )
        .await;
        return Ok(projection);
    }
    let work = RegisteredWorkApplicationServicesV1::attach(&registered.database)
        .map_err(|_| workflow_runtime_unavailable())?;
    let authority = tracedecay_domain::WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .map_err(|_| workflow_not_found())?;
    let plans = projection
        .fan_out_plans()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut cancelled_children = false;
    for plan in plans {
        if plan.authority != authority {
            return Err(workflow_not_found());
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
            .map_err(|_| workflow_invalid_request())?;
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
            )
            .await?;
        }

        let mut active = 0usize;
        let mut terminal = Vec::new();
        let mut terminal_planned = Vec::new();
        let mut known = std::collections::BTreeSet::new();
        let released = projection
            .released_fan_out_attempts()
            .iter()
            .filter(|identity| {
                plan.children
                    .iter()
                    .any(|child| &child.attempt_identity == *identity)
            })
            .cloned()
            .collect::<Vec<_>>();
        let statuses = attempt_statuses(
            &work,
            context,
            released
                .iter()
                .map(|identity| projection.active_fan_out_attempt(identity).clone())
                .collect(),
        )
        .await?;
        for (identity, status) in released.into_iter().zip(statuses) {
            match status {
                Ok(attempt) if attempt.is_terminal() => {
                    known.insert(identity.clone());
                    terminal_planned.push(identity);
                    terminal.push(attempt);
                }
                Ok(attempt)
                    if attempt.state()
                        == tracedecay_domain::WorkAttemptStateV1::RecoveryRequired =>
                {
                    // The prior dispatch has an unknown outcome. Keep its
                    // workflow slot active without launching the same child
                    // identity again.
                    known.insert(identity);
                    active += 1;
                }
                Ok(_) => {
                    known.insert(identity);
                    active += 1;
                }
                Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => active += 1,
                Err(_) => return Err(workflow_runtime_unavailable()),
            }
        }
        let newly_settled = terminal_planned
            .iter()
            .filter(|&identity| !projection.settled_fan_out_attempts().contains(identity))
            .cloned()
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
            )
            .await?;
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
                &projection,
                &plan,
                &terminal_planned,
                observed_at,
            )
            .await?;
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
                    settle_workflow_fan_out(services, &projection, &plan, &terminal, observed_at)
                        .await?;
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
            )
            .await?;
        }
        for child in &plan.children {
            if !projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
                || known.contains(&child.attempt_identity)
            {
                continue;
            }
            let product_binding = workflow_product_binding()?;
            let product = RegisteredWorkProductServicesV1::attach(
                &registered.database,
                product_binding.clone(),
            )
            .map_err(|_| workflow_runtime_unavailable())?;
            admit_workflow_child(registered, &product, context, child, observed_at).await?;
            let revisions = workflow_product_revision_pins(registered)?;
            let attempt = product
                .start_attempt(
                    context.clone(),
                    product_binding,
                    revisions,
                    registered.work_topology_policy.clone(),
                    tracedecay_contracts::StartWorkAttemptCommand {
                        task_id: child.task_id.clone(),
                        run_id: child.attempt_identity.run_id().clone(),
                        attempt_id: child.attempt_identity.attempt_id().clone(),
                        operation: plan.operation.clone(),
                        execution_snapshot: plan.execution_snapshot.clone(),
                        worktree_root: project_root
                            .to_str()
                            .ok_or_else(workflow_invalid_request)?
                            .to_owned(),
                        reference: plan.reference.clone(),
                        commit: plan.commit.clone(),
                        instructions: child.instructions.clone(),
                        effect_state: plan.effect_state,
                        occurred_at: observed_at,
                    },
                )
                .await
                .map_err(|_| workflow_runtime_unavailable())?;
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
                settle_workflow_fan_out(services, &projection, &plan, &terminal, observed_at)
                    .await?;
        }
    }
    super::workflow_census::persist_workflow_fan_out_census(
        registered,
        services,
        context,
        &projection,
        observed_at,
        observability_producer,
    )
    .await;
    if cancelled_children || projection.sequence() != initial_sequence {
        super::publish_committed_task_activity_in_background(
            registered.database.clone(),
            project_root.to_path_buf(),
            None,
        );
    }
    Ok(projection)
}

async fn attempt_statuses(
    work: &RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    identities: Vec<WorkAttemptIdentityV1>,
) -> Result<Vec<Result<tracedecay_domain::WorkAttemptV1, ApplicationProblem>>, ApplicationProblem> {
    let context = context.clone();
    work.read(move |work| {
        Ok(identities
            .iter()
            .map(|identity| {
                work.attempts().status(
                    &context,
                    &tracedecay_contracts::WorkAttemptStatusRequestV1 {
                        task_id: identity.task_id().clone(),
                        run_id: identity.run_id().clone(),
                        attempt_id: identity.attempt_id().clone(),
                    },
                )
            })
            .collect())
    })
    .await
    .map_err(|_| workflow_runtime_unavailable())
}

async fn reconcile_cancelled_fan_out(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    projection: tracedecay_domain::WorkflowRunProjection,
    observed_at: UtcMicros,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
) -> Result<(tracedecay_domain::WorkflowRunProjection, bool), ApplicationProblem> {
    let work = RegisteredWorkApplicationServicesV1::attach(&registered.database)
        .map_err(|_| workflow_runtime_unavailable())?;
    let mut all_terminal = true;
    let mut cancelled_children = false;
    for plan in projection.fan_out_plans().values() {
        cancelled_children |= request_fan_out_cancellation(
            context,
            &work,
            attempt_processes,
            &projection,
            plan,
            &[],
            observed_at,
        )
        .await?;
        let released = plan
            .children
            .iter()
            .filter(|child| {
                projection
                    .released_fan_out_attempts()
                    .contains(&child.attempt_identity)
            })
            .map(|child| {
                projection
                    .active_fan_out_attempt(&child.attempt_identity)
                    .clone()
            })
            .collect();
        for status in attempt_statuses(&work, context, released).await? {
            match status {
                Ok(attempt) => all_terminal &= attempt.is_terminal(),
                Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => {}
                Err(_) => return Err(workflow_runtime_unavailable()),
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
        .ok_or_else(workflow_invalid_request)?;
    let projection = apply_scheduler_command(
        services,
        &projection,
        tracedecay_domain::WorkflowRunCommand::ReconcileCancelled,
        "cancel",
        plan_digest,
        observed_at,
    )
    .await?;
    Ok((projection, cancelled_children))
}

async fn settle_workflow_fan_out(
    services: &RegisteredWorkflowApplicationServicesV1,
    projection: &tracedecay_domain::WorkflowRunProjection,
    plan: &tracedecay_domain::WorkflowFanOutPlanV1,
    attempts: &[tracedecay_domain::WorkAttemptV1],
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
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
        .ok_or_else(workflow_invalid_request)?;
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
                            .ok_or_else(workflow_runtime_unavailable)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                tracedecay_domain::WorkflowStepOutput::new(output_name.clone(), artifacts)
                    .map_err(|_| workflow_runtime_unavailable())
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let step = projection
        .step(&plan.step_id)
        .ok_or_else(workflow_invalid_request)?;
    let placement_digest = step
        .placement_receipt()
        .map(|receipt| receipt.placement_digest().clone())
        .ok_or_else(workflow_runtime_unavailable)?;
    let effect_digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-fan-out-terminal.v1",
        &plan.plan_digest,
        attempts,
        &outputs,
    ))
    .map_err(|_| workflow_runtime_unavailable())?;
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
    .map_err(|_| workflow_runtime_unavailable())?;
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
    .await
}

async fn request_fan_out_cancellation(
    context: &RequestContext,
    services: &RegisteredWorkApplicationServicesV1,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
    projection: &tracedecay_domain::WorkflowRunProjection,
    plan: &tracedecay_domain::WorkflowFanOutPlanV1,
    terminal: &[tracedecay_domain::WorkAttemptIdentityV1],
    occurred_at: UtcMicros,
) -> Result<bool, ApplicationProblem> {
    let children = plan
        .children
        .iter()
        .filter(|child| {
            !terminal
                .iter()
                .any(|identity| identity == &child.attempt_identity)
        })
        .collect::<Vec<_>>();
    let statuses = attempt_statuses(
        services,
        context,
        children
            .iter()
            .map(|child| {
                projection
                    .active_fan_out_attempt(&child.attempt_identity)
                    .clone()
            })
            .collect(),
    )
    .await?;
    let mut cancelled_children = false;
    for (child, status) in children.into_iter().zip(statuses) {
        let Ok(attempt) = status else {
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
        let identity = projection.active_fan_out_attempt(&child.attempt_identity);
        let request_id = tracedecay_domain::WorkCancellationRequestId::new(format!(
            "workflow-fail-fast:{}:{}",
            plan.plan_digest.as_str(),
            child.attempt_identity.attempt_id().as_str()
        ))
        .map_err(|_| workflow_runtime_unavailable())?;
        let cancelled = match services
            .cancel_attempt(
                context.clone(),
                tracedecay_contracts::CancelWorkAttemptCommand {
                    task_id: child.task_id.clone(),
                    run_id: identity.run_id().clone(),
                    attempt_id: identity.attempt_id().clone(),
                    request_id,
                    occurred_at,
                },
            )
            .await
        {
            Ok(cancelled) => cancelled,
            Err(problem) if is_not_cancellable(&problem) => continue,
            Err(_) => return Err(workflow_runtime_unavailable()),
        };
        attempt_processes.signal_cancellation(&context.scope().worktree_id, cancelled.identity());
        cancelled_children = true;
    }
    Ok(cancelled_children)
}

pub(crate) fn is_not_cancellable(problem: &ApplicationProblem) -> bool {
    problem
        .diagnostic()
        .is_some_and(|diagnostic| diagnostic.code == "application.work-attempt.not-cancellable")
}

async fn apply_scheduler_command(
    services: &RegisteredWorkflowApplicationServicesV1,
    projection: &tracedecay_domain::WorkflowRunProjection,
    command: tracedecay_domain::WorkflowRunCommand,
    operation: &str,
    plan_digest: &ManifestDigest,
    occurred_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, ApplicationProblem> {
    let digest = canonical_sha256(&(
        "tracedecay.daemon.workflow-scheduler-command.v1",
        projection.run_id(),
        projection.sequence(),
        operation,
        plan_digest,
        &command,
    ))
    .map_err(|_| workflow_runtime_unavailable())?;
    let command_id = tracedecay_domain::WorkCommandId::new(format!(
        "workflow-scheduler-{operation}:{}",
        digest.as_str()
    ))
    .map_err(|_| workflow_runtime_unavailable())?;
    services
        .apply_run(
            projection.run_id().clone(),
            projection.sequence(),
            command,
            tracedecay_domain::WorkflowRunEventContext {
                command_id,
                input_digest: digest,
                occurred_at,
            },
        )
        .await
        .map_err(workflow_run_problem)
}

async fn admit_workflow_child(
    registered: &RegisteredWorkRuntime,
    product: &RegisteredWorkProductServicesV1,
    context: &RequestContext,
    child: &tracedecay_domain::WorkflowFanOutChildPlanV1,
    occurred_at: UtcMicros,
) -> Result<(), ApplicationProblem> {
    let selection = tracedecay_contracts::WorkProductSelectionScopeV1::relations(
        [
            tracedecay_contracts::WorkProductAuthorizedRelationScopeV1::Repository {
                project_id: context.scope().project_id.clone(),
                repository_id: context.scope().repository_id.clone(),
            },
        ]
        .into_iter()
        .collect(),
    )
    .map_err(|_| workflow_invalid_request())?;
    let binding = workflow_product_binding()?;
    let graph =
        current_workflow_product_graph(registered, context, &selection, occurred_at).await?;
    match graph {
        None => {
            let initial_graph = tracedecay_domain::WorkProductGraphV1::new(
                tracedecay_domain::WorkGraphVersionV1::initial(),
                vec![child.initiative.clone()],
                vec![child.plan.clone()],
                vec![child.milestone.clone()],
                vec![child.item.clone()],
            )
            .map_err(|_| workflow_invalid_request())?;
            let revisions = workflow_product_revision_pins(registered)?;
            product
                .mutate(
                    context.clone(),
                    binding.clone(),
                    tracedecay_contracts::WorkProductMutationRequestV1::Create(
                        tracedecay_contracts::CreateWorkProductRequestV1 {
                            selection: selection.clone(),
                            initial_graph,
                            mutation: tracedecay_contracts::WorkProductMutationIdentityV1 {
                                expected_authority:
                                    tracedecay_contracts::WorkProductExpectedAuthorityV1::NoPriorGraph,
                                command_id: child.create_command_id.clone(),
                                causation_event_id: None,
                                evidence: Vec::new(),
                                occurred_at,
                                revisions: revisions.clone(),
                            },
                        },
                    ),
                    revisions,
                )
                .await
                .map_err(|_| workflow_runtime_unavailable())?;
        }
        Some(graph) if graph.item(&child.task_id).is_none() => {
            apply_workflow_child_product_mutation(
                registered,
                product,
                context,
                &binding,
                selection.clone(),
                tracedecay_contracts::WorkProductChangeDraftV1::CreateTask {
                    initiative: child.initiative.clone(),
                    plan: child.plan.clone(),
                    milestone: child.milestone.clone(),
                    item: Box::new(child.item.clone()),
                },
                child.create_command_id.clone(),
                occurred_at,
            )
            .await?;
        }
        Some(graph) if !workflow_child_task_matches(&graph, child) => {
            return Err(workflow_invalid_request());
        }
        Some(_) => {}
    }

    let graph = current_workflow_product_graph(registered, context, &selection, occurred_at)
        .await?
        .ok_or_else(workflow_runtime_unavailable)?;
    let item = graph
        .item(&child.task_id)
        .ok_or_else(workflow_runtime_unavailable)?;
    match item.accepted_proposal() {
        None => {
            let proposal = if child.proposal.based_on_version() == graph.version() {
                child.proposal.clone()
            } else {
                child.proposal.clone().rebased_onto(graph.version())
            };
            apply_workflow_child_product_mutation(
                registered,
                product,
                context,
                &binding,
                selection.clone(),
                tracedecay_contracts::WorkProductChangeDraftV1::DecideProposal {
                    proposal,
                    disposition: tracedecay_domain::WorkProposalDispositionV1::Accepted,
                },
                child.proposal_command_id.clone(),
                occurred_at,
            )
            .await?;
        }
        Some(proposal_id)
            if proposal_id == child.proposal.proposal_id()
                && graph.proposal_decisions().iter().any(|decision| {
                    decision.proposal().proposal_id() == child.proposal.proposal_id()
                        && decision.disposition()
                            == &tracedecay_domain::WorkProposalDispositionV1::Accepted
                }) => {}
        Some(_) => return Err(workflow_invalid_request()),
    }

    let graph = current_workflow_product_graph(registered, context, &selection, occurred_at)
        .await?
        .ok_or_else(workflow_runtime_unavailable)?;
    let item = graph
        .item(&child.task_id)
        .ok_or_else(workflow_runtime_unavailable)?;
    if !item.is_execution_admitted() {
        apply_workflow_child_product_mutation(
            registered,
            product,
            context,
            &binding,
            selection,
            tracedecay_contracts::WorkProductChangeDraftV1::AdmitExecution {
                task_id: child.task_id.clone(),
            },
            child.admit_command_id.clone(),
            occurred_at,
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn workflow_product_binding()
-> Result<tracedecay_contracts::WorkProductBindingV1, ApplicationProblem> {
    Ok(tracedecay_contracts::WorkProductBindingV1::new(
        CapabilityId::new("capability.work.mutate_graph")
            .map_err(|_| workflow_runtime_unavailable())?,
        UseCaseId::new("use-case.work.mutate_graph").map_err(|_| workflow_runtime_unavailable())?,
    ))
}

pub(crate) fn workflow_product_revision_pins(
    registered: &RegisteredWorkRuntime,
) -> Result<tracedecay_contracts::WorkProductRevisionPinsV1, ApplicationProblem> {
    super::preparation::current_work_product_revision_pins(registered)
        .map_err(|_| workflow_runtime_unavailable())
}

async fn current_workflow_product_graph(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    selection: &tracedecay_contracts::WorkProductSelectionScopeV1,
    observed_at: UtcMicros,
) -> Result<Option<tracedecay_domain::WorkProductGraphV1>, ApplicationProblem> {
    let work = RegisteredWorkApplicationServicesV1::attach(&registered.database)
        .map_err(|_| workflow_runtime_unavailable())?;
    let (registered, context, selection) = (registered.clone(), context.clone(), selection.clone());
    work.read(move |_| {
        let product = RegisteredWorkProductServicesV1::attach(
            &registered.database,
            workflow_product_binding()?,
        )
        .map_err(|_| workflow_runtime_unavailable())?;
        match product.reads().read_graph(
            &context,
            tracedecay_contracts::WorkGraphReadRequestV1::current(selection, observed_at),
        ) {
            Ok(tracedecay_contracts::WorkGraphReadV1::Current { snapshot, .. }) => {
                Ok(Some(snapshot.graph().clone()))
            }
            Ok(tracedecay_contracts::WorkGraphReadV1::Absent { .. }) => Ok(None),
            Ok(_) | Err(_) => Err(workflow_runtime_unavailable()),
        }
    })
    .await
    .map_err(|_| workflow_runtime_unavailable())
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
async fn apply_workflow_child_product_mutation(
    registered: &RegisteredWorkRuntime,
    product: &RegisteredWorkProductServicesV1,
    context: &RequestContext,
    binding: &tracedecay_contracts::WorkProductBindingV1,
    selection: tracedecay_contracts::WorkProductSelectionScopeV1,
    change: tracedecay_contracts::WorkProductChangeDraftV1,
    command_id: tracedecay_domain::WorkCommandId,
    occurred_at: UtcMicros,
) -> Result<(), ApplicationProblem> {
    let revisions = workflow_product_revision_pins(registered)?;
    let work = RegisteredWorkApplicationServicesV1::attach(&registered.database)
        .map_err(|_| workflow_runtime_unavailable())?;
    let mutation = {
        let (registered, context, binding, revisions) = (
            registered.clone(),
            context.clone(),
            binding.clone(),
            revisions.clone(),
        );
        work.read(move |_| {
            RegisteredWorkProductServicesV1::attach(&registered.database, binding.clone())
                .map_err(|_| workflow_runtime_unavailable())?
                .mutations()
                .prepare_mutation(
                    &context,
                    &binding,
                    tracedecay_contracts::PrepareWorkProductMutationRequestV1 {
                        selection,
                        change,
                        causation_event_id: None,
                        evidence: Vec::new(),
                    },
                    command_id,
                    occurred_at,
                    revisions,
                )
                .map_err(|error| {
                    tracing::warn!(
                        ?error,
                        stage = "prepare_mutation",
                        "workflow child product mutation prepare failed"
                    );
                    workflow_runtime_unavailable()
                })
        })
        .await
        .map_err(|_| workflow_runtime_unavailable())?
    };
    product
        .mutate(context.clone(), binding.clone(), mutation, revisions)
        .await
        .map(|_| ())
        .map_err(|error| {
            tracing::warn!(
                ?error,
                stage = "mutate",
                "workflow child product mutation apply failed"
            );
            workflow_runtime_unavailable()
        })
}

pub(crate) async fn reconcile_workflow_fan_out_after_attempt(
    registered: &RegisteredWorkRuntime,
    attempt_processes: Arc<super::super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: &Path,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
    observability_producer: Option<
        Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    >,
) {
    let Ok(services) = RegisteredWorkflowApplicationServicesV1::attach(&registered.database).await
    else {
        return;
    };
    let run_id = identity.run_id().clone();
    let Ok(Ok(projection)) = services
        .read(move |services| Ok(services.effects().projection(&run_id)))
        .await
    else {
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
        now_micros(),
        attempt_processes,
        project_root,
        observability_producer,
    )
    .await
    {
        tracing::warn!(?problem, "workflow fan-out reconciliation did not complete");
    }
}

pub(super) async fn synchronize_fan_out_run_controls(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    projection: &tracedecay_domain::WorkflowRunProjection,
    paused: bool,
    occurred_at: UtcMicros,
) -> Result<(), ApplicationProblem> {
    let work = RegisteredWorkApplicationServicesV1::attach(&registered.database)
        .map_err(|_| workflow_runtime_unavailable())?;
    let children = projection
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .filter(|child| {
            projection
                .released_fan_out_attempts()
                .contains(&child.attempt_identity)
        })
        .map(|child| tracedecay_contracts::WorkRunControlRequestV1 {
            task_id: child.task_id.clone(),
            run_id: child.attempt_identity.run_id().clone(),
        })
        .collect::<Vec<_>>();
    let readings = {
        let (context, children) = (context.clone(), children.clone());
        work.read(move |work| {
            Ok(children
                .iter()
                .map(|request| work.run_control().read(&context, request))
                .collect::<Vec<_>>())
        })
        .await
        .map_err(|_| workflow_runtime_unavailable())?
    };
    for (child, reading) in children.into_iter().zip(readings) {
        let reading = match reading {
            Ok(reading) => reading,
            Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => continue,
            Err(_) => return Err(workflow_runtime_unavailable()),
        };
        let transition = match (paused, reading) {
            (true, tracedecay_contracts::WorkRunControlReadingV1::Uncontrolled { .. }) => {
                work.pause_run(
                    context.clone(),
                    tracedecay_contracts::PauseWorkRunCommand {
                        task_id: child.task_id,
                        run_id: child.run_id,
                        reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                        expected_authority_version: None,
                        occurred_at,
                    },
                )
                .await
            }
            (true, tracedecay_contracts::WorkRunControlReadingV1::Controlled { control, .. })
                if control.state() == tracedecay_domain::WorkRunControlStateV1::Running =>
            {
                work.pause_run(
                    context.clone(),
                    tracedecay_contracts::PauseWorkRunCommand {
                        task_id: child.task_id,
                        run_id: child.run_id,
                        reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                        expected_authority_version: Some(control.authority().get()),
                        occurred_at,
                    },
                )
                .await
            }
            (false, tracedecay_contracts::WorkRunControlReadingV1::Controlled { control, .. })
                if control.state() == tracedecay_domain::WorkRunControlStateV1::Paused =>
            {
                work.resume_run(
                    context.clone(),
                    tracedecay_contracts::ResumeWorkRunCommand {
                        task_id: child.task_id,
                        run_id: child.run_id,
                        reason: tracedecay_domain::WorkRunControlReasonV1::OperatorRequest,
                        expected_authority_version: control.authority().get(),
                        occurred_at,
                    },
                )
                .await
            }
            _ => continue,
        };
        transition.map_err(|_| workflow_runtime_unavailable())?;
    }
    Ok(())
}
