//! Work application invocation dispatch.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::{CancellationContext, Deadline};
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::CapabilityId;

use crate::daemon_contract::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1,
};

use super::super::work_attempt_exec::WorkAttemptProcessRegistryV1;
use super::attempt_operations;
use super::evidence_retrieval;
use super::intelligence;
use super::leak_adjudication::adjudicate_leak;
use super::preparation;
use super::{
    RegisteredWorkRuntime, complete_work_effect, complete_work_read, observe_placement_target,
    offer_work_blocked_interval_receipts, work_product_problem, work_projection_problem,
    work_request_context, work_topology_problem, work_topology_unavailable_problem,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<
        Arc<tracedecay_usecases::observability::BoundedObservabilityProducerV1>,
    >,
    project_root: Option<PathBuf>,
    request_id: String,
    request: WorkApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
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
    let services = match registered.database.work_application_services() {
        Ok(services) => services,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let response = match request {
        WorkApplicationInvocationV1::Snapshot(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .projections()
                .snapshot(&context, request.page_size)
                .map_err(work_projection_problem),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Snapshot,
        ),
        WorkApplicationInvocationV1::Delta(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .projections()
                .delta(&context, &request.cursor, request.page_size)
                .map_err(work_projection_problem),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Delta,
        ),
        WorkApplicationInvocationV1::GenerateProposal(request) => {
            let result = intelligence::generate_proposal(
                &registered,
                &context,
                capability,
                &use_case,
                request,
            );
            if let Ok(proposal) = &result {
                let _ = tracedecay_usecases::observability::record_task_intelligence_decision(
                    observability_producer.as_deref(),
                    proposal,
                    observed_at,
                );
            }
            complete_work_read(
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
                WorkApplicationOutcomeV1::GenerateProposal,
            )
        }
        WorkApplicationInvocationV1::Create(command) => {
            let Ok(capability_id) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability_id, use_case.clone());
            let created = registered
                .database
                .work_product_services(binding.clone())
                .map_err(|_| {
                    tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable
                })
                .and_then(|product| {
                    preparation::current_work_product_revision_pins(&registered)
                        .map_err(|_| {
                            tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable
                        })
                        .and_then(|revisions| {
                            if command.mutation.revisions != revisions {
                                return Err(
                                    tracedecay_application::WorkProductApplicationErrorV1::RevisionConflict,
                                );
                            }
                            product
                                .mutations()
                                .create_task(&context, &binding, command)
                        })
                })
                .map_err(work_product_problem);
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                created,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::Create,
            )
        }
        WorkApplicationInvocationV1::ReplanDependencies(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().replan_dependencies(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ReplanDependencies,
        ),
        WorkApplicationInvocationV1::ReviewProposal(request) => {
            let proposal_ref = request.proposal.proposal_id().as_str().to_owned();
            let command_ref = request.mutation.command_id.as_str().to_owned();
            let disposition = request.disposition.clone();
            let occurred_at = request.mutation.occurred_at;
            let result = preparation::decide_product_proposal(
                &registered,
                &context,
                *capability,
                &use_case,
                request,
                false,
            );
            if result.is_ok() {
                let disposition = match disposition {
                    tracedecay_domain::WorkProposalDispositionV1::Rejected => {
                        Some(tracedecay_application::ReviewProposalDispositionV1::Rejected)
                    }
                    tracedecay_domain::WorkProposalDispositionV1::Superseded => {
                        Some(tracedecay_application::ReviewProposalDispositionV1::Superseded)
                    }
                    tracedecay_domain::WorkProposalDispositionV1::Accepted => None,
                };
                let _ = tracedecay_usecases::observability::record_reliance_decision(
                    observability_producer.as_deref(),
                    &proposal_ref,
                    &command_ref,
                    disposition,
                    occurred_at,
                );
            }
            complete_work_effect(
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
                WorkApplicationOutcomeV1::ReviewProposal,
            )
        }
        WorkApplicationInvocationV1::AcceptProposal(command) => {
            let proposal_ref = command.proposal.proposal_id().as_str().to_owned();
            let command_ref = command.mutation.command_id.as_str().to_owned();
            let occurred_at = command.mutation.occurred_at;
            let result = preparation::decide_product_proposal(
                &registered,
                &context,
                *capability,
                &use_case,
                command,
                true,
            );
            if result.is_ok() {
                let _ = tracedecay_usecases::observability::record_reliance_decision(
                    observability_producer.as_deref(),
                    &proposal_ref,
                    &command_ref,
                    None,
                    occurred_at,
                );
            }
            complete_work_effect(
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
                WorkApplicationOutcomeV1::AcceptProposal,
            )
        }
        WorkApplicationInvocationV1::AdmitExecution(command) => {
            let result = CapabilityId::new(*capability)
                .map_err(|_| {
                    work_product_problem(
                        tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                    )
                })
                .and_then(|capability| {
                    let binding = tracedecay_application::WorkProductBindingV1::new(
                        capability,
                        use_case.clone(),
                    );
                    registered
                        .database
                        .work_product_services(binding.clone())
                        .map_err(|_| {
                            work_product_problem(
                                tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
                            )
                        })
                        .and_then(|product| {
                            let revisions =
                                preparation::current_work_product_revision_pins(&registered)?;
                            if command.mutation.revisions != revisions {
                                return Err(work_product_problem(
                                    tracedecay_application::WorkProductApplicationErrorV1::RevisionConflict,
                                ));
                            }
                            product
                                .mutations()
                                .admit_execution(&context, &binding, command)
                                .map_err(work_product_problem)
                        })
                });
            complete_work_effect(
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
                WorkApplicationOutcomeV1::AdmitExecution,
            )
        }
        WorkApplicationInvocationV1::AcceptTask(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().accept_task(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AcceptTask,
        ),
        WorkApplicationInvocationV1::StartAttempt(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::start_attempt(
                &registered,
                &services,
                binding,
                &attempt_processes,
                observability_producer.as_ref(),
                project_root.as_ref(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                observed_at,
                deadline,
                command,
            )
        }
        WorkApplicationInvocationV1::Synthesize(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::synthesize(
                &registered,
                &services,
                binding,
                &attempt_processes,
                observability_producer.as_ref(),
                project_root.as_ref(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                observed_at,
                deadline,
                command,
            )
        }
        WorkApplicationInvocationV1::AttemptStatus(request) => attempt_operations::attempt_status(
            &registered,
            &services,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            observed_at,
            deadline,
            request,
        ),
        WorkApplicationInvocationV1::CancelAttempt(command) => attempt_operations::cancel_attempt(
            &registered,
            &services,
            &attempt_processes,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            observed_at,
            deadline,
            command,
        ),
        WorkApplicationInvocationV1::RetryAttempt(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::retry_attempt(
                &registered,
                &services,
                binding,
                &attempt_processes,
                observability_producer.as_ref(),
                project_root.as_ref(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                observed_at,
                deadline,
                command,
            )
        }
        WorkApplicationInvocationV1::ListAttempts(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.attempts().list(&context, &request, |authority| {
                let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                match services.topology().verified_snapshot(authority, cancelled) {
                    Ok(topology) => {
                        let task_count = u32::try_from(topology.task_count()).map_err(|_| {
                            work_topology_unavailable_problem(
                                "the verified topology task count overflowed",
                            )
                        })?;
                        Ok(
                            tracedecay_application::WorkAttemptTopologyStateV1::Verified(
                                tracedecay_application::WorkAttemptTopologyBindingV1 {
                                    generation: topology.generation().as_str().to_owned(),
                                    task_count,
                                },
                            ),
                        )
                    }
                    Err(error) => work_topology_problem(error),
                }
            }),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ListAttempts,
        ),
        WorkApplicationInvocationV1::ExecutionHistory(request) => intelligence::execution_history(
            &registered,
            &services,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            observed_at,
            deadline,
            request,
        ),
        WorkApplicationInvocationV1::HydrateArtifacts(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .artifact_hydration()
                .hydrate(&context, &request, |authority| {
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    match services.topology().verified_snapshot(authority, cancelled) {
                        Ok(topology) => {
                            let task_count =
                                u32::try_from(topology.task_count()).map_err(|_| {
                                    work_topology_unavailable_problem(
                                        "the verified topology task count overflowed",
                                    )
                                })?;
                            Ok(
                                tracedecay_application::WorkAttemptTopologyStateV1::Verified(
                                    tracedecay_application::WorkAttemptTopologyBindingV1 {
                                        generation: topology.generation().as_str().to_owned(),
                                        task_count,
                                    },
                                ),
                            )
                        }
                        Err(error) => work_topology_problem(error),
                    }
                }),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::HydrateArtifacts,
        ),
        WorkApplicationInvocationV1::RetrieveEvidence(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case.clone(),
            input_digest,
            evidence_retrieval::retrieve(&registered, &context, capability, use_case, request)
                .await,
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::RetrieveEvidence,
        ),
        WorkApplicationInvocationV1::Topology(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            tracedecay_application::execution_topology_view(
                services.attempts(),
                services.placement(),
                &registered.work_topology_policy,
                &context,
                &request,
                |authority| {
                    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    match services.topology().verified_snapshot(authority, cancelled) {
                        Ok(topology) => {
                            let task_count =
                                u32::try_from(topology.task_count()).map_err(|_| {
                                    work_topology_unavailable_problem(
                                        "the verified topology task count overflowed",
                                    )
                                })?;
                            Ok(
                                tracedecay_application::WorkAttemptTopologyStateV1::Verified(
                                    tracedecay_application::WorkAttemptTopologyBindingV1 {
                                        generation: topology.generation().as_str().to_owned(),
                                        task_count,
                                    },
                                ),
                            )
                        }
                        Err(error) => work_topology_problem(error),
                    }
                },
            ),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Topology,
        ),
        WorkApplicationInvocationV1::TopologyMetrics(request) => {
            let observations =
                tracedecay_usecases::observability::RegisteredObservabilityPortV1::new(
                    &registered.database,
                );
            let metrics = tracedecay_application::execution_topology_rollup_metrics(
                &observations,
                &observations,
                &context,
                &request,
            )
            .await;
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                metrics,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::TopologyMetrics,
            )
        }
        WorkApplicationInvocationV1::PrepareDuplicateAdjudication(request) => {
            let prepared = preparation::prepare_duplicate_adjudication(
                &services,
                &context,
                request,
                &canonical_request_id,
                observed_at,
            );
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                prepared,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PrepareDuplicateAdjudication,
            )
        }
        WorkApplicationInvocationV1::AdjudicateDuplicate(command) => {
            let authority = match tracedecay_domain::WorkAuthority::new(
                context.scope().project_id.clone(),
                context.scope().repository_id.clone(),
                context.scope().worktree_id.clone(),
                context.actor().clone(),
                context.grant().digest.clone(),
            ) {
                Ok(authority) => authority,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::InvalidRequest,
                    );
                }
            };
            let adjudicated = services
                .duplicate_adjudications()
                .adjudicate(&context, command);
            if let Ok(outcome) = &adjudicated {
                let _observation =
                    tracedecay_usecases::observability::record_work_duplicate_observation(
                        observability_producer.as_deref(),
                        context.scope().project_id.as_str(),
                        &authority,
                        outcome.receipt(),
                    );
            }
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                adjudicated,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::AdjudicateDuplicate,
            )
        }
        WorkApplicationInvocationV1::AdjudicateLeak(command) => {
            let adjudicated = adjudicate_leak(
                &registered,
                &services,
                attempt_processes.as_ref(),
                &context,
                command,
                observed_at,
                &deadline,
            )
            .await;
            if let Ok(outcome) = &adjudicated {
                let _ = tracedecay_usecases::observability::record_work_leak_observation(
                    observability_producer.as_deref(),
                    context.scope().project_id.as_str(),
                    outcome.receipt(),
                );
            }
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                adjudicated,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::AdjudicateLeak,
            )
        }
        WorkApplicationInvocationV1::ResumeAttempts(command) => {
            attempt_operations::resume_attempts(
                &registered,
                &services,
                &attempt_processes,
                observability_producer.as_ref(),
                project_root.as_ref(),
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                observed_at,
                deadline,
                command,
            )
        }
        WorkApplicationInvocationV1::Views(request) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
            let product_services = match registered.database.work_product_services(binding) {
                Ok(services) => services,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                }
            };
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                product_services
                    .reads()
                    .read_graph(&context, request)
                    .map_err(work_product_problem),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::Views,
            )
        }
        WorkApplicationInvocationV1::Experience(request) => {
            intelligence::experience(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                observed_at,
                deadline,
                *capability,
                request,
            )
            .await
        }
        WorkApplicationInvocationV1::CompareProposal(request) => intelligence::compare_proposal(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            observed_at,
            deadline,
            *capability,
            request,
        ),
        WorkApplicationInvocationV1::PrepareGraphMutation(request) => {
            let prepared = preparation::prepare_graph_mutation(
                &registered,
                &context,
                *capability,
                &use_case,
                request,
                &canonical_request_id,
                observed_at,
            );
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                prepared,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PrepareGraphMutation,
            )
        }
        WorkApplicationInvocationV1::MutateGraph(request) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
            let product_services = match registered.database.work_product_services(binding.clone())
            {
                Ok(services) => services,
                Err(_) => {
                    return DaemonInvocationResponse::problem(
                        request_id,
                        DaemonInvocationProblem::Unavailable,
                    );
                }
            };
            let mutated = preparation::current_work_product_revision_pins(&registered).and_then(
                |revisions| {
                    product_services
                        .mutations()
                        .mutate(&context, &binding, request, &revisions)
                        .map_err(work_product_problem)
                },
            );
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                mutated,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::MutateGraph,
            )
        }
        WorkApplicationInvocationV1::PauseRun(command) => {
            let transition = services.run_control().pause_with_receipt(&context, command);
            let open_receipts = transition
                .as_ref()
                .map(|transition| transition.blocked_intervals.clone())
                .unwrap_or_default();
            let response = complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                transition.map(|transition| transition.control),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PauseRun,
            );
            offer_work_blocked_interval_receipts(
                observability_producer.as_deref(),
                context.scope().project_id.as_str(),
                &open_receipts,
            );
            response
        }
        WorkApplicationInvocationV1::ResumeRun(command) => {
            let transition = services
                .run_control()
                .resume_with_receipt(&context, command);
            let settled_receipts = transition
                .as_ref()
                .map(|transition| transition.blocked_intervals.clone())
                .unwrap_or_default();
            let response = complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                transition.map(|transition| transition.control),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::ResumeRun,
            );
            offer_work_blocked_interval_receipts(
                observability_producer.as_deref(),
                context.scope().project_id.as_str(),
                &settled_receipts,
            );
            response
        }
        WorkApplicationInvocationV1::RunControl(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.run_control().read(&context, &request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::RunControl,
        ),
        WorkApplicationInvocationV1::PlacementPreflight(request) => {
            let placement_root = project_root.clone();
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                services.placement().preflight(&context, request, |target| {
                    observe_placement_target(placement_root.as_deref(), target, observed_at)
                }),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PlacementPreflight,
            )
        }
        WorkApplicationInvocationV1::AdmitPlacement(command) => {
            let placement_root = project_root.clone();
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                services
                    .placement()
                    .admit_placement(&context, command, |target| {
                        observe_placement_target(placement_root.as_deref(), target, observed_at)
                    }),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::AdmitPlacement,
            )
        }
        WorkApplicationInvocationV1::PlacementStatus(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.placement().status(&context, &request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::PlacementStatus,
        ),
        WorkApplicationInvocationV1::ReleasePlacement(command) => {
            let placement_root = project_root.clone();
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                services.placement().release(&context, command, |target| {
                    observe_placement_target(placement_root.as_deref(), target, observed_at)
                }),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::ReleasePlacement,
            )
        }
    };
    response
}
