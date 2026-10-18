//! Work application invocation dispatch.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracing::Instrument;

use tracedecay_contracts::{CancellationContext, Deadline, WorkAttemptListOperationV1};
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::CapabilityId;

use tracedecay_daemon_protocol::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1,
};

use super::super::work_attempt_exec::WorkAttemptProcessRegistryV1;
use super::attempt_operations;
use super::evidence_retrieval;
use super::intelligence;
use super::job;
use super::leak_adjudication::adjudicate_leak;
use super::preparation;
use super::{
    RegisteredWorkRuntime, complete_work_effect, complete_work_read, observe_placement_target,
    offer_work_blocked_interval_receipts, work_product_problem, work_request_context,
};

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(name = "daemon.service.work.dispatch", level = "trace", skip_all)]
pub(super) async fn dispatch_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    observability_producer: Option<
        Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>,
    >,
    project_root: Option<PathBuf>,
    request_id: String,
    request: WorkApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    // A guided job's steps are their own Work invocations under this request's
    // cancellation.
    let job_cancellation = cancellation.clone();
    let Some((_, capability, use_case)) = tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
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
    let services = match tracedecay_application::work::RegisteredWorkApplicationServicesV1::attach(
        &registered.database,
    ) {
        Ok(services) => services,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };

    match request {
        WorkApplicationInvocationV1::GenerateProposal(request) => {
            let result = intelligence::generate_proposal(
                &registered,
                &services,
                &context,
                capability,
                &use_case,
                request,
            )
            .await;
            if let Ok(proposal) = &result {
                let _ = tracedecay_application::observability::record_task_intelligence_decision(
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
            let created = tracing::Instrument::instrument(
                preparation::mutate_product(
                    &registered,
                    &context,
                    capability,
                    &use_case,
                    tracedecay_contracts::WorkProductMutationRequestV1::CreateTask(Box::new(
                        command,
                    )),
                ),
                tracing::trace_span!("daemon.service.work.create"),
            )
            .await;
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
        WorkApplicationInvocationV1::ReviewProposal(request) => {
            let proposal_ref = request.proposal.proposal_id().as_str().to_owned();
            let command_ref = request.mutation.command_id.as_str().to_owned();
            let disposition = request.disposition;
            let occurred_at = request.mutation.occurred_at;
            let result = tracing::Instrument::instrument(
                preparation::mutate_product(
                    &registered,
                    &context,
                    capability,
                    &use_case,
                    tracedecay_contracts::WorkProductMutationRequestV1::DecideProposal(
                        request.into(),
                    ),
                ),
                tracing::trace_span!("daemon.service.work.review_proposal"),
            )
            .await;
            if result.is_ok() {
                let _ = tracedecay_application::observability::record_reliance_decision(
                    observability_producer.as_deref(),
                    &proposal_ref,
                    &command_ref,
                    Some(disposition),
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
            let result = tracing::Instrument::instrument(
                preparation::mutate_product(
                    &registered,
                    &context,
                    capability,
                    &use_case,
                    tracedecay_contracts::WorkProductMutationRequestV1::DecideProposal(
                        command.into(),
                    ),
                ),
                tracing::trace_span!("daemon.service.work.accept_proposal"),
            )
            .await;
            if result.is_ok() {
                let _ = tracedecay_application::observability::record_reliance_decision(
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
            let result = tracing::Instrument::instrument(
                preparation::admit_execution(&registered, &context, capability, &use_case, command),
                tracing::trace_span!("daemon.service.work.admit_execution"),
            )
            .await;
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
        WorkApplicationInvocationV1::StartAttempt(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::start_attempt(
                &registered,
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
            .await
        }
        WorkApplicationInvocationV1::Synthesize(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::synthesize(
                &registered,
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
            .await
        }
        WorkApplicationInvocationV1::AttemptStatus(request) => {
            attempt_operations::attempt_status(
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
            )
            .await
        }
        WorkApplicationInvocationV1::CancelAttempt(command) => {
            attempt_operations::cancel_attempt(
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
            )
            .await
        }
        WorkApplicationInvocationV1::RetryAttempt(command) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
            attempt_operations::retry_attempt(
                &registered,
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
            .await
        }
        WorkApplicationInvocationV1::ListAttempts(request) => {
            let listed = {
                let (registered, context, use_case) =
                    (registered.clone(), context.clone(), use_case.clone());
                services
                    .read(move |services| {
                        services.attempts().list(
                            &context,
                            WorkAttemptListOperationV1::ListAttempts,
                            &request,
                            || {
                                preparation::current_work_product_attempt_topology(
                                    &registered,
                                    &context,
                                    capability,
                                    &use_case,
                                    observed_at,
                                )
                            },
                        )
                    })
                    .instrument(tracing::trace_span!("daemon.service.work.list_attempts"))
                    .await
            };
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                listed,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::ListAttempts,
            )
        }
        WorkApplicationInvocationV1::ExecutionHistory(request) => {
            intelligence::execution_history(
                &registered,
                &services,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                capability,
                use_case,
                input_digest,
                observed_at,
                deadline,
                request,
            )
            .await
        }
        WorkApplicationInvocationV1::HydrateArtifacts(request) => {
            let hydrated = {
                let (registered, context, use_case) =
                    (registered.clone(), context.clone(), use_case.clone());
                services
                    .read(move |services| {
                        services
                            .artifact_hydration()
                            .hydrate(&context, &request, |_authority| {
                                preparation::current_work_product_attempt_topology(
                                    &registered,
                                    &context,
                                    capability,
                                    &use_case,
                                    observed_at,
                                )
                            })
                    })
                    .instrument(tracing::trace_span!(
                        "daemon.service.work.hydrate_artifacts"
                    ))
                    .await
            };
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                hydrated,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::HydrateArtifacts,
            )
        }
        WorkApplicationInvocationV1::RetrieveEvidence(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case.clone(),
            input_digest,
            {
                let (registered, context) = (registered.clone(), context.clone());
                evidence_retrieval::retrieve(registered, context, capability, use_case, request)
                    .await
            },
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::RetrieveEvidence,
        ),
        WorkApplicationInvocationV1::Topology(request) => {
            let view = {
                let (registered, context, use_case) =
                    (registered.clone(), context.clone(), use_case.clone());
                services
                    .read(move |services| {
                        tracedecay_contracts::execution_topology_view(
                            services.attempts(),
                            services.placement(),
                            &registered.work_topology_policy,
                            &context,
                            &request,
                            || {
                                preparation::current_work_product_attempt_topology(
                                    &registered,
                                    &context,
                                    capability,
                                    &use_case,
                                    observed_at,
                                )
                            },
                        )
                    })
                    .instrument(tracing::trace_span!("daemon.service.work.topology"))
                    .await
            };
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                view,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::Topology,
            )
        }
        WorkApplicationInvocationV1::TopologyMetrics(request) => {
            let observations =
                tracedecay_application::observability::RegisteredObservabilityPortV1::new(
                    &registered.database,
                );
            let metrics: Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                tracedecay_contracts::ExecutionTopologyMetricsV1,
                                tracedecay_contracts::ApplicationProblem,
                            >,
                        > + Send,
                >,
            > = Box::pin(tracedecay_contracts::execution_topology_rollup_metrics(
                &observations,
                &observations,
                &context,
                &request,
            ));
            let metrics = tracing::Instrument::instrument(
                metrics,
                tracing::trace_span!("daemon.service.work.topology_metrics"),
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
            let prepared = {
                let (registered, context, use_case, canonical_request_id) = (
                    registered.clone(),
                    context.clone(),
                    use_case.clone(),
                    canonical_request_id.clone(),
                );
                services
                    .read(move |services| {
                        preparation::prepare_duplicate_adjudication(
                            &registered,
                            services,
                            &context,
                            capability,
                            &use_case,
                            request,
                            &canonical_request_id,
                            observed_at,
                        )
                    })
                    .instrument(tracing::trace_span!(
                        "daemon.service.work.prepare_duplicate"
                    ))
                    .await
            };
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
            let adjudicated = tracing::Instrument::instrument(
                async {
                    let evidence = {
                        let (registered, context, use_case) =
                            (registered.clone(), context.clone(), use_case.clone());
                        services
                            .read(move |_| {
                                preparation::current_duplicate_adjudication_evidence(
                                    &registered,
                                    &context,
                                    capability,
                                    &use_case,
                                    observed_at,
                                )
                            })
                            .await?
                    };
                    services
                        .adjudicate_duplicate(context.clone(), command, evidence)
                        .await
                },
                tracing::trace_span!("daemon.service.work.adjudicate_duplicate"),
            )
            .await;
            if let Ok(outcome) = &adjudicated {
                let _observation =
                    tracedecay_application::observability::record_work_duplicate_observation(
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
            let adjudicated: Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                tracedecay_contracts::WorkLeakAdjudicationOutcomeV1,
                                tracedecay_contracts::ApplicationProblem,
                            >,
                        > + Send,
                >,
            > = Box::pin(adjudicate_leak(
                &registered,
                &services,
                attempt_processes.as_ref(),
                &context,
                command,
                observed_at,
                &deadline,
            ));
            let adjudicated = adjudicated.await;
            if let Ok(outcome) = &adjudicated {
                let _ = tracedecay_application::observability::record_work_leak_observation(
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
            .await
        }
        WorkApplicationInvocationV1::Views(request) => {
            let Ok(capability) = CapabilityId::new(*capability) else {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            };
            let binding =
                tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
            let product_services =
                match tracedecay_application::work::RegisteredWorkProductServicesV1::attach_with_attempt_authority(
                    &registered.database,
                    binding,
                    registered.authority.clone(),
                ) {
                    Ok(services) => services,
                    Err(_) => {
                        return DaemonInvocationResponse::problem(
                            request_id,
                            DaemonInvocationProblem::Unavailable,
                        );
                    }
                };
            let read_context = context.clone();
            let graph = services
                .read(move |_| {
                    product_services
                        .reads()
                        .read_graph(&read_context, request)
                        .map_err(work_product_problem)
                })
                .instrument(tracing::trace_span!("daemon.service.work.views"))
                .await;
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                graph,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::Views,
            )
        }
        WorkApplicationInvocationV1::Experience(request) => {
            let experienced: Pin<Box<dyn Future<Output = DaemonInvocationResponse> + Send>> =
                Box::pin(intelligence::experience(
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
                    capability,
                    request,
                ));
            experienced.await
        }
        WorkApplicationInvocationV1::CompareProposal(request) => {
            intelligence::compare_proposal(
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
                capability,
                request,
            )
            .await
        }
        WorkApplicationInvocationV1::PrepareGraphMutation(request) => {
            let prepared = {
                let (registered, context, use_case, canonical_request_id) = (
                    registered.clone(),
                    context.clone(),
                    use_case.clone(),
                    canonical_request_id.clone(),
                );
                services
                    .read(move |_| {
                        preparation::prepare_graph_mutation(
                            &registered,
                            &context,
                            capability,
                            &use_case,
                            request,
                            &canonical_request_id,
                            observed_at,
                        )
                    })
                    .instrument(tracing::trace_span!(
                        "daemon.service.work.prepare_graph_mutation"
                    ))
                    .await
            };
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
            let mutated = tracing::Instrument::instrument(
                preparation::mutate_product(&registered, &context, capability, &use_case, request),
                tracing::trace_span!("daemon.service.work.mutate_graph"),
            )
            .await;
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
            let transition = tracing::Instrument::instrument(
                services.pause_run(context.clone(), command),
                tracing::trace_span!("daemon.service.work.pause_run"),
            )
            .await;
            let open_receipts = transition
                .as_ref()
                .map_or_default(|transition| transition.blocked_intervals.clone());
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
                &registered.durable_write_signal,
                observability_producer.as_deref(),
                context.scope().project_id.as_str(),
                &open_receipts,
            );
            response
        }
        WorkApplicationInvocationV1::ResumeRun(command) => {
            let transition = tracing::Instrument::instrument(
                services.resume_run(context.clone(), command),
                tracing::trace_span!("daemon.service.work.resume_run"),
            )
            .await;
            let settled_receipts = transition
                .as_ref()
                .map_or_default(|transition| transition.blocked_intervals.clone());
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
                &registered.durable_write_signal,
                observability_producer.as_deref(),
                context.scope().project_id.as_str(),
                &settled_receipts,
            );
            response
        }
        WorkApplicationInvocationV1::RunControl(request) => {
            let read_context = context.clone();
            let reading = services
                .read(move |services| services.run_control().read(&read_context, &request))
                .instrument(tracing::trace_span!("daemon.service.work.run_control"))
                .await;
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                reading,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::RunControl,
            )
        }
        WorkApplicationInvocationV1::PlacementPreflight(request) => {
            let (placement_root, read_context) = (project_root.clone(), context.clone());
            let preflight = services
                .read(move |services| {
                    services
                        .placement()
                        .preflight(&read_context, request, |target| {
                            observe_placement_target(placement_root.as_deref(), target, observed_at)
                        })
                })
                .instrument(tracing::trace_span!(
                    "daemon.service.work.placement_preflight"
                ))
                .await;
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                preflight,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PlacementPreflight,
            )
        }
        WorkApplicationInvocationV1::AdmitPlacement(command) => {
            let placement_root = project_root.clone();
            let admitted = tracing::Instrument::instrument(
                services.admit_placement(context.clone(), command, move |target| {
                    observe_placement_target(placement_root.as_deref(), target, observed_at)
                }),
                tracing::trace_span!("daemon.service.work.admit_placement"),
            )
            .await;
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                admitted,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::AdmitPlacement,
            )
        }
        WorkApplicationInvocationV1::PlacementStatus(request) => {
            let read_context = context.clone();
            let status = services
                .read(move |services| services.placement().status(&read_context, &request))
                .instrument(tracing::trace_span!("daemon.service.work.placement_status"))
                .await;
            complete_work_read(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                status,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::PlacementStatus,
            )
        }
        WorkApplicationInvocationV1::ReleasePlacement(command) => {
            let placement_root = project_root.clone();
            let released = tracing::Instrument::instrument(
                services.release_placement(context.clone(), command, move |target| {
                    observe_placement_target(placement_root.as_deref(), target, observed_at)
                }),
                tracing::trace_span!("daemon.service.work.release_placement"),
            )
            .await;
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                released,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::ReleasePlacement,
            )
        }
        WorkApplicationInvocationV1::AdvanceJob(request) => {
            let advanced = match preparation::repository_selection(&context) {
                Ok(selection) => {
                    let host = job::WorkJobHost {
                        registered: registered.clone(),
                        attempt_processes,
                        observability_producer,
                        project_root,
                        selection,
                        deadline: deadline.clone(),
                        cancellation: job_cancellation,
                    };
                    {
                        let advanced: Pin<
                            Box<
                                dyn Future<
                                        Output = Result<
                                            tracedecay_contracts::AdvanceWorkJobResultV1,
                                            tracedecay_contracts::ApplicationProblem,
                                        >,
                                    > + Send,
                            >,
                        > = Box::pin(job::advance_job(
                            &host, &context, capability, &use_case, request,
                        ));
                        advanced.await
                    }
                }
                Err(problem) => Err(problem),
            };
            complete_work_effect(
                &registered,
                request_id,
                &context,
                canonical_request_id,
                operation_key,
                use_case,
                input_digest,
                advanced,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::AdvanceJob,
            )
        }
    }
}
