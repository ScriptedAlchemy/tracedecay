//! Work/workflow application daemon invocation handlers.

use super::*;

pub(super) fn application_problem(
    request_id: String,
    problem: ApplicationProblem,
) -> DaemonInvocationResponse {
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ApplicationProblem { problem },
    )
}

pub(super) fn concealed_application_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    )
}

/// Retryable state for an admitted project route whose per-project runtime or
/// service registration has not finished mounting behind the core open
/// publication. Concealing this window as not-found would misreport an
/// authenticated project the caller is standing in.
pub(super) fn runtime_mounting_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.surface.unavailable".to_owned(),
            message: "The project runtime for this operation is still mounting".to_owned(),
        }),
    )
}

/// Dispatches one Work application invocation and, when that invocation
/// committed a Work mutation, publishes the Task-family activity pulse that
/// backs the dashboard's `task_activity` stream.
///
/// The pulse is raised here rather than inside [`complete_work_effect`] because
/// effect completion is synchronous while publication awaits the registered
/// observation store. Keeping the synchronous dispatch in
/// [`dispatch_work_application`] also keeps that call's service handles out of
/// this future.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
    project_root: Option<PathBuf>,
    request_id: String,
    request: WorkApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let activity_database = Arc::clone(&registered.database);
    let activity_root = project_root.clone();
    let mutates = work_invocation_mutates(&request);
    let response = dispatch_work_application(
        registered,
        attempt_processes,
        project_root,
        request_id,
        request,
        observed_at,
        deadline,
        cancellation,
    );
    // Only a mutation that reached a Work outcome committed: an application
    // problem or a daemon problem leaves the graph exactly as it was, and a
    // pulse with no project root has no coalescing bucket to land in.
    if mutates
        && matches!(
            response.outcome,
            DaemonInvocationOutcome::WorkApplication { .. }
        )
        && let Some(project_root) = activity_root.as_deref()
    {
        crate::application::event_lane::publish(
            &activity_database,
            crate::application::event_lane::ActivityFamilyV1::Task,
            project_root,
            None,
            1,
            work_activity_detail(&response.outcome),
        )
        .await;
    }
    response
}

/// Exactly the invocations the dispatcher completes through
/// [`complete_work_effect`]. The match is exhaustive, so a new invocation has to
/// declare whether it mutates Work state before this compiles, and the read
/// arms can never fall through into the mutation pulse.
const fn work_invocation_mutates(request: &WorkApplicationInvocationV1) -> bool {
    match request {
        WorkApplicationInvocationV1::Snapshot(_)
        | WorkApplicationInvocationV1::Delta(_)
        | WorkApplicationInvocationV1::GenerateProposal(_)
        | WorkApplicationInvocationV1::AttemptStatus(_)
        | WorkApplicationInvocationV1::ListAttempts(_)
        | WorkApplicationInvocationV1::Views(_)
        | WorkApplicationInvocationV1::RunControl(_)
        | WorkApplicationInvocationV1::PlacementPreflight(_)
        | WorkApplicationInvocationV1::PlacementStatus(_) => false,
        WorkApplicationInvocationV1::Create(_)
        | WorkApplicationInvocationV1::ReplanDependencies(_)
        | WorkApplicationInvocationV1::ReviewProposal(_)
        | WorkApplicationInvocationV1::AcceptProposal(_)
        | WorkApplicationInvocationV1::AdmitExecution(_)
        | WorkApplicationInvocationV1::AttachRuntimeEvidence(_)
        | WorkApplicationInvocationV1::AcceptTask(_)
        | WorkApplicationInvocationV1::StartAttempt(_)
        | WorkApplicationInvocationV1::CancelAttempt(_)
        | WorkApplicationInvocationV1::ResumeAttempts(_)
        | WorkApplicationInvocationV1::PauseRun(_)
        | WorkApplicationInvocationV1::ResumeRun(_)
        | WorkApplicationInvocationV1::AdmitPlacement(_)
        | WorkApplicationInvocationV1::ReleasePlacement(_) => true,
    }
}

/// Observes one placement target through the native Git authority.
///
/// Plan 36 owns Git evidence, so this reads status rather than deciding
/// anything: the application's placement service turns these counts into typed
/// blockers. Every failure path answers `readable: false` instead of an empty
/// reading, because a target that could not be read is not a clean one.
///
/// `unique_commits` is deliberately left unmeasured (`None`). Reachability is
/// not something status can answer, and Plan 32 forbids cleaning when the state
/// is "unknown" — so an unmeasured target quarantines on release rather than
/// being assumed worthless. Measuring it is the follow-up that turns a
/// quarantine into a clean release.
fn observe_placement_target(
    project_root: Option<&std::path::Path>,
    target: &tracedecay_domain::WorkPlacementTargetV1,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkPlacementObservationV1, ApplicationProblem> {
    use tracedecay_domain::git::{GitChangeKindV1, GitStatusEntryV1};

    let unreadable = tracedecay_domain::WorkPlacementObservationV1 {
        dirty_tracked_paths: 0,
        untracked_paths: 0,
        unique_commits: None,
        readable: false,
        // Exclusivity is storage's answer, not the filesystem's; the service
        // overwrites this from the durable placement rows.
        active_holder: false,
        network_required: false,
        observed_at,
    };
    // A managed placement is judged at its own root; an in-place or unmanaged
    // placement is judged at the project the request resolved to. Neither is
    // inferred from a current directory.
    let root = match target.root() {
        Some(root) => std::path::PathBuf::from(root),
        None => match project_root {
            Some(root) => root.to_path_buf(),
            None => return Ok(unreadable),
        },
    };
    let Ok(repository) =
        tracedecay_runtime_core::git_repository::GitRepositoryAuthority::discover(&root)
    else {
        return Ok(unreadable);
    };
    let Ok(status) = repository.status() else {
        return Ok(unreadable);
    };
    let mut dirty_tracked_paths = 0u32;
    let mut untracked_paths = 0u32;
    for entry in &status.entries {
        match entry {
            GitStatusEntryV1::Tracked(tracked) => {
                if tracked.index != GitChangeKindV1::Unmodified
                    || tracked.worktree != GitChangeKindV1::Unmodified
                {
                    dirty_tracked_paths = dirty_tracked_paths.saturating_add(1);
                }
            }
            GitStatusEntryV1::Untracked { .. } => {
                untracked_paths = untracked_paths.saturating_add(1);
            }
            // Ignored paths are not the caller's uncommitted work.
            GitStatusEntryV1::Ignored { .. } => {}
        }
    }
    Ok(tracedecay_domain::WorkPlacementObservationV1 {
        dirty_tracked_paths,
        untracked_paths,
        readable: true,
        ..unreadable
    })
}

/// Attempt state carried by a committed attempt mutation. Attempt states are
/// the only detail vocabulary the canonical `task` activity payload admits, so
/// every other Work mutation publishes an undetailed pulse rather than a label
/// the observation store would strip.
fn work_activity_detail(outcome: &DaemonInvocationOutcome) -> Option<&'static str> {
    let DaemonInvocationOutcome::WorkApplication { outcome, .. } = outcome else {
        return None;
    };
    let attempt = match outcome {
        WorkApplicationOutcomeV1::StartAttempt(ApplicationOutcome::Effect(effect))
        | WorkApplicationOutcomeV1::CancelAttempt(ApplicationOutcome::Effect(effect)) => {
            effect.payload.as_ref()?
        }
        _ => return None,
    };
    Some(match attempt.state() {
        tracedecay_domain::WorkAttemptStateV1::Leased => "leased",
        tracedecay_domain::WorkAttemptStateV1::Running => "running",
        tracedecay_domain::WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        tracedecay_domain::WorkAttemptStateV1::CancellationAcknowledged => {
            "cancellation_acknowledged"
        }
        tracedecay_domain::WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        tracedecay_domain::WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        tracedecay_domain::WorkAttemptStateV1::Succeeded => "succeeded",
        tracedecay_domain::WorkAttemptStateV1::Failed => "failed",
        tracedecay_domain::WorkAttemptStateV1::TimedOut => "timed_out",
        tracedecay_domain::WorkAttemptStateV1::Cancelled => "cancelled",
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
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
    match request {
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
        WorkApplicationInvocationV1::GenerateProposal(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().generate_proposal(
                &context,
                registered.configuration_digest.clone(),
                request,
            ),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::GenerateProposal,
        ),
        WorkApplicationInvocationV1::Create(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().create(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::Create,
        ),
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
        WorkApplicationInvocationV1::ReviewProposal(request) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().review_proposal(&context, request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ReviewProposal,
        ),
        WorkApplicationInvocationV1::AcceptProposal(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().accept_proposal(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AcceptProposal,
        ),
        WorkApplicationInvocationV1::AdmitExecution(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.commands().admit_execution(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AdmitExecution,
        ),
        WorkApplicationInvocationV1::AttachRuntimeEvidence(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services
                .commands()
                .attach_runtime_evidence(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AttachRuntimeEvidence,
        ),
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
            // Plan 32 ("One runtime, run control, and effect budget"): "pause
            // and cancellation fence new reservations". The decision lives in
            // the run-control service; this dispatch only composes the two
            // application authorities in the order the fence requires, exactly
            // as the attempt list composes the topology authority below.
            let started = match services.run_control().admit_reservation(
                &context,
                &command.task_id,
                &command.run_id,
            ) {
                Ok(()) => services.attempts().start(&context, command),
                Err(problem) => Err(problem),
            };
            if let (Ok(attempt), Some(project_root)) = (&started, project_root.as_ref())
                && attempt.state() == tracedecay_domain::WorkAttemptStateV1::Leased
            {
                super::work_attempt_exec::spawn_attempt_execution(
                    registered.clone(),
                    Arc::clone(&attempt_processes),
                    project_root.clone(),
                    attempt.clone(),
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
                started,
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::StartAttempt,
            )
        }
        WorkApplicationInvocationV1::AttemptStatus(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.attempts().status(&context, &request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::AttemptStatus,
        ),
        WorkApplicationInvocationV1::CancelAttempt(command) => {
            let cancelled = services.attempts().request_cancellation(&context, command);
            if let Ok(attempt) = &cancelled {
                attempt_processes.signal_cancellation(attempt.identity());
            }
            complete_work_effect(
                &registered,
                request_id,
                &context,
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
        WorkApplicationInvocationV1::ListAttempts(request) => complete_work_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.attempts().list(&context, &request, |authority| {
                // The invocation admission already refused cancelled requests
                // and this dispatch path carries no live cancellation signal,
                // so the bounded topology read runs to completion.
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
        WorkApplicationInvocationV1::ResumeAttempts(command) => {
            let report = services.attempts().resume(&context, &command);
            if let (Ok(report), Some(project_root)) = (&report, project_root.as_ref()) {
                for attempt in &report.recovery_required {
                    super::work_attempt_exec::spawn_attempt_execution(
                        registered.clone(),
                        Arc::clone(&attempt_processes),
                        project_root.clone(),
                        attempt.clone(),
                    );
                }
            }
            complete_work_effect(
                &registered,
                request_id,
                &context,
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
        WorkApplicationInvocationV1::Views(request) => {
            // The work-product graph authority is bound to the operation that
            // reads it: the read service refuses a context that does not carry
            // this operation's own capability and use case, so the binding is
            // composed from the row dispatch already resolved rather than from
            // a second identity table.
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
                    .map_err(work_product_read_problem),
                observed_at,
                deadline,
                WorkApplicationOutcomeV1::Views,
            )
        }
        WorkApplicationInvocationV1::PauseRun(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.run_control().pause(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::PauseRun,
        ),
        WorkApplicationInvocationV1::ResumeRun(command) => complete_work_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            services.run_control().resume(&context, command),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ResumeRun,
        ),
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
    }
}

/// Reports a work-product graph read exactly as the read service typed it.
///
/// Every arm restates a state the service already decided; none of them
/// substitutes an empty success for an absent or unreadable graph, because an
/// empty reading and an unavailable authority are different answers to the
/// caller. Absence and denial share the concealed
/// `not_found_or_not_authorized` answer so that probing an owner cannot reveal
/// which of the two it is.
fn work_product_read_problem(
    error: tracedecay_application::WorkProductApplicationErrorV1,
) -> ApplicationProblem {
    use tracedecay_application::WorkProductApplicationErrorV1 as Error;

    match error {
        Error::NotAuthorized | Error::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        Error::Cancelled => ApplicationProblem::cancelled_before_admission(),
        Error::TimedOut => ApplicationProblem::timed_out_before_admission(),
        Error::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.invalid_graph_read".to_owned(),
                message: "The Work graph read request is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        Error::VersionConflict | Error::ReconciliationRequired => {
            ApplicationProblem::stale(SafeDiagnostic {
                code: "work.graph_version_conflict".to_owned(),
                message: "The Work graph version changed while it was being read".to_owned(),
            })
        }
        Error::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "work.graph_idempotency_conflict".to_owned(),
                message: "The Work graph request key was reused with different input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        Error::GraphAuthorityUnavailable
        | Error::EventAuthorityUnavailable
        | Error::EvidenceAuthorityUnavailable
        | Error::ProposalAuthorityUnavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.graph_authority_unavailable".to_owned(),
            message: "The Work graph authority is unavailable".to_owned(),
        }),
    }
}

/// Mints the daemon-owned request context under which background attempt
/// execution persists transitions. The authority and scope are exactly the
/// registered runtime's; only the deadline is the runtime's own, because the
/// provider process outlives the request that started it.
pub(super) fn work_background_context(
    registered: &RegisteredWorkRuntime,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
) -> Result<RequestContext, ApplicationContractError> {
    const BACKGROUND_DEADLINE_MICROS: i64 = 86_400_000_000;
    let request_id = RequestId::new(format!(
        "work-attempt-exec-{}-{}-{}",
        identity.task_id().as_str(),
        identity.run_id().as_str(),
        identity.attempt_id().as_str()
    ))?;
    let deadline = Deadline::new(UtcMicros(
        current_micros()
            .0
            .saturating_add(BACKGROUND_DEADLINE_MICROS),
    ))?;
    let cancellation = CancellationContext::active(format!(
        "work-attempt-exec-{}",
        identity.attempt_id().as_str()
    ))?;
    RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        request_id,
        deadline,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_workflow_application(
    registered: RegisteredWorkRuntime,
    request_id: String,
    request: WorkflowApplicationInvocation,
    _observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let observed_at = crate::daemon_client::invocation_now_micros();
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
    let services = match registered.database.workflow_application_services() {
        Ok(services) => services,
        Err(error) => {
            return DaemonInvocationResponse::problem(request_id, workflow_storage_problem(&error));
        }
    };

    match request {
        WorkflowApplicationInvocation::RegisterDefinition(request) => {
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
        }
        WorkflowApplicationInvocation::ActivateDefinition(request) => {
            let prepared = WorkflowEffectPreparedV1::activate_definition(
                input_digest.clone(),
                WorkflowDefinitionLifecycleCommand {
                    definition_id: request.definition_id,
                    definition_version: request.definition_version,
                    operation: WorkflowLifecycleOperation::Activate,
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
        }
        WorkflowApplicationInvocation::RetireDefinition(request) => {
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
        }
        WorkflowApplicationInvocation::RejectDefinition(request) => {
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
        }
        WorkflowApplicationInvocation::ValidateDefinition(request) => complete_workflow_read(
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
        ),
        WorkflowApplicationInvocation::GetDefinition(request) => complete_workflow_read(
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
        ),
        WorkflowApplicationInvocation::ListDefinitions(_) => complete_workflow_read(
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
        ),
        WorkflowApplicationInvocation::DefinitionHistory(request) => complete_workflow_read(
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
        ),
        WorkflowApplicationInvocation::DiffDefinition(request) => complete_workflow_read(
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
        ),
        WorkflowApplicationInvocation::HandoffIssue(request) => {
            let prepared = match TaskHandoffToken::new(request.secret).and_then(|token| {
                prepare_task_handoff_issue(&context, request.scope, &token, observed_at)
            }) {
                Ok(grant) => WorkflowEffectPreparedV1::handoff_issue(input_digest.clone(), grant),
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
        }
        WorkflowApplicationInvocation::HandoffRedeem(request) => {
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
        }
        WorkflowApplicationInvocation::StartRun(request) => {
            let result = start_workflow_run(
                &registered,
                &services,
                &context,
                request,
                &input_digest,
                observed_at,
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
        }
        WorkflowApplicationInvocation::PauseRun(request) => {
            let result = apply_workflow_run_command(
                &services,
                &request.run_id,
                request.expected_sequence,
                tracedecay_domain::WorkflowRunCommand::Pause,
                request.command_id,
                &input_digest,
                observed_at,
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
                WorkflowApplicationOutcome::PauseRun,
            )
        }
        WorkflowApplicationInvocation::ResumeRun(request) => {
            let result = apply_workflow_run_command(
                &services,
                &request.run_id,
                request.expected_sequence,
                tracedecay_domain::WorkflowRunCommand::Resume,
                request.command_id,
                &input_digest,
                observed_at,
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
                WorkflowApplicationOutcome::ResumeRun,
            )
        }
        WorkflowApplicationInvocation::CancelRun(request) => {
            let result = cancel_workflow_run(&services, request, &input_digest, observed_at);
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
        }
        WorkflowApplicationInvocation::GetRun(request) => complete_workflow_read(
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
        ),
    }
}

/// Admits a workflow run from an Active definition version.
///
/// Every admission digest is derived by the daemon from its registered
/// environment: live policy/configuration digests, the shipped workflow
/// catalog digest, the pinned work topology policy digest, and the digest of
/// the provider registry built from the request's registration. A definition
/// pinned against a different environment is a typed staleness denial, and a
/// registry that cannot place the definition's entry step denies admission
/// before any event is journaled.
fn start_workflow_run(
    registered: &RegisteredWorkRuntime,
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_application::WorkflowRunStartRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let definition = services
        .definitions()
        .get(&request.definition_id, request.definition_version)
        .map_err(workflow_coordination_problem)?;
    if definition.project_id() != &context.scope().project_id {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    let disposition = services
        .definitions()
        .disposition(&request.definition_id, request.definition_version)
        .map_err(workflow_coordination_problem)?;
    if disposition.state != tracedecay_application::WorkflowDefinitionLifecycleState::Active {
        return Err(DaemonInvocationProblem::InvalidRequest);
    }
    let registry = tracedecay_application::WorkflowProviderRegistry::new(
        registered.configuration_digest.clone(),
        vec![request.provider],
    )
    .map_err(workflow_placement_problem)?;
    let topology = &registered.work_topology_policy;
    let topology_digest = topology
        .compute_digest()
        .map_err(|_| DaemonInvocationProblem::Unavailable)?
        .0;
    let entry_step = definition
        .steps()
        .first()
        .ok_or(DaemonInvocationProblem::InvalidRequest)?
        .step_id
        .clone();
    tracedecay_application::WorkflowProviderPlacementService::new(registry.clone())
        .place(
            &tracedecay_application::WorkflowTopologyPlacementRequest {
                run_id: request.run_id.clone(),
                step_id: entry_step,
                configuration_digest: registered.configuration_digest.clone(),
                topology_digest: topology_digest.clone(),
            },
            topology,
        )
        .map_err(workflow_placement_problem)?;
    let admission = tracedecay_application::WorkflowAdmissionSnapshot {
        policy_digest: registered.policy_digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: tracedecay_application::work_executable_catalog_digest()
            .map_err(|_| DaemonInvocationProblem::Unavailable)?,
        topology_digest,
        provider_registry_digest: registry.digest().clone(),
    };
    tracedecay_application::WorkflowRunService::new(services.effects().clone())
        .admit(
            request.run_id,
            definition,
            admission,
            tracedecay_domain::WorkflowRunEventContext {
                command_id: request.command_id,
                input_digest: input_digest.clone(),
                occurred_at: observed_at,
            },
        )
        .map_err(workflow_run_problem)
}

fn apply_workflow_run_command(
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    run_id: &tracedecay_domain::RunId,
    expected_sequence: u64,
    command: tracedecay_domain::WorkflowRunCommand,
    command_id: tracedecay_domain::WorkCommandId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    tracedecay_application::WorkflowRunService::new(services.effects().clone())
        .apply(
            run_id,
            expected_sequence,
            command,
            tracedecay_domain::WorkflowRunEventContext {
                command_id,
                input_digest: input_digest.clone(),
                occurred_at: observed_at,
            },
        )
        .map_err(workflow_run_problem)
}

/// Requests cooperative cancellation and, when no step is still running,
/// immediately reconciles the run to its terminal `Cancelled` state under a
/// command identity derived from the caller's, so replays settle identically.
fn cancel_workflow_run(
    services: &crate::global_db::RegisteredWorkflowApplicationServicesV1,
    request: tracedecay_application::WorkflowRunCancelRequest,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem> {
    let reconcile_command_id = tracedecay_domain::WorkCommandId::try_from(format!(
        "{}.reconcile",
        request.command_id.as_str()
    ))
    .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let cancelling = apply_workflow_run_command(
        services,
        &request.run_id,
        request.expected_sequence,
        tracedecay_domain::WorkflowRunCommand::RequestCancellation,
        request.command_id,
        input_digest,
        observed_at,
    )?;
    let any_step_running = cancelling.definition().steps().iter().any(|step| {
        cancelling.step(&step.step_id).is_some_and(|projected| {
            projected.status() == tracedecay_domain::WorkflowStepStatus::Running
        })
    });
    if any_step_running {
        return Ok(cancelling);
    }
    apply_workflow_run_command(
        services,
        &request.run_id,
        cancelling.sequence(),
        tracedecay_domain::WorkflowRunCommand::ReconcileCancelled,
        reconcile_command_id,
        input_digest,
        observed_at,
    )
}

fn workflow_run_problem(
    error: tracedecay_application::WorkflowRunServiceError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowRunServiceError::PolicyDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::ConfigurationDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::CatalogDigestMismatch
        | tracedecay_application::WorkflowRunServiceError::State(_) => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowRunServiceError::Storage(error) => {
            workflow_run_storage_problem(error)
        }
    }
}

fn workflow_run_storage_problem(
    error: tracedecay_application::WorkflowRunStorageError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowRunStorageError::NotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        tracedecay_application::WorkflowRunStorageError::VersionConflict
        | tracedecay_application::WorkflowRunStorageError::IdempotencyConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowRunStorageError::InvalidHistory => {
            DaemonInvocationProblem::ResetRequired
        }
        tracedecay_application::WorkflowRunStorageError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

fn workflow_placement_problem(
    error: tracedecay_application::WorkflowProviderPlacementError,
) -> DaemonInvocationProblem {
    match error {
        tracedecay_application::WorkflowProviderPlacementError::InvalidRegistry
        | tracedecay_application::WorkflowProviderPlacementError::ConfigurationDigestMismatch
        | tracedecay_application::WorkflowProviderPlacementError::TopologyDigestMismatch
        | tracedecay_application::WorkflowProviderPlacementError::InvalidTopology => {
            DaemonInvocationProblem::InvalidRequest
        }
        tracedecay_application::WorkflowProviderPlacementError::Unavailable => {
            DaemonInvocationProblem::Unavailable
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_workflow_run_effect(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<tracedecay_domain::WorkflowRunProjection, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>,
    ) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse {
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_command_effect(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(outcome) => wrap(outcome),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_workflow_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, DaemonInvocationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkflowApplicationOutcome,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let outcome = match work_evidence_packet(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(evidence) => wrap(ApplicationOutcome::Evidence(evidence)),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_journaled_workflow_effect(
    registered: &RegisteredWorkRuntime,
    authority: &impl WorkflowEffectAuthorityPortV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    prepared: WorkflowEffectPreparedV1,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> DaemonInvocationResponse {
    let operation = match workflow_effect_operation(operation_key) {
        Some(operation) => operation,
        None => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let receipt_context = match workflow_effect_receipt_context(
        registered,
        context,
        operation_key,
        use_case,
        &input_digest,
        observed_at,
    ) {
        Ok(receipt_context) => receipt_context,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let receipt_binding_digest = match receipt_context.binding_digest() {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let idempotency_digest = match canonical_sha256(&(
        "tracedecay.daemon.workflow-effect-idempotency.v1",
        operation_key,
        &input_digest,
        context.actor(),
        context.scope(),
        receipt_binding_digest,
    )) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let suffix = match idempotency_digest.as_str().strip_prefix("sha256:") {
        Some(suffix) => suffix,
        None => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let idempotency_key = match IdempotencyKey::new(format!("workflow.{operation_key}.{suffix}")) {
        Ok(key) => key,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let identity = match WorkflowEffectIdentityV1::new(
        operation,
        idempotency_key,
        canonical_request_id,
        context.actor().clone(),
        context.scope().clone(),
        input_digest,
        observed_at,
        deadline,
        receipt_context,
    ) {
        Ok(identity) => identity,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let prepared = if identity.deadline().is_elapsed_at(current_micros()) {
        WorkflowEffectPreparedV1::problem(
            identity.input_digest().clone(),
            WorkflowEffectProblemV1::TimedOut,
        )
    } else {
        prepared
    };
    let record = match authority.execute_effect(&identity, &prepared, current_micros()) {
        Ok(record) => record,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let Some(terminal) = record.terminal() else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    let outcome = match workflow_effect_outcome(terminal) {
        Ok(outcome) => outcome,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkflowApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

fn workflow_effect_receipt_context(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: &ManifestDigest,
    observed_at: UtcMicros,
) -> Result<WorkflowEffectReceiptContextV1, ApplicationContractError> {
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work input digest",
        },
    )?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let catalog_digest =
        canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            }
        })?;
    let privacy_digest = canonical_sha256(&(
        "tracedecay.work.privacy.v1",
        context.scope(),
        context.grant().disclosure,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work privacy digest",
    })?;
    Ok(WorkflowEffectReceiptContextV1::new(
        use_case,
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        authority,
        expected_state,
        registered.configuration_digest.clone(),
        catalog_digest,
        privacy_digest,
    ))
}

fn workflow_effect_operation(operation_key: &str) -> Option<WorkflowEffectOperationV1> {
    match operation_key {
        "register_definition" => Some(WorkflowEffectOperationV1::RegisterDefinition),
        "activate_definition" => Some(WorkflowEffectOperationV1::ActivateDefinition),
        "retire_definition" => Some(WorkflowEffectOperationV1::RetireDefinition),
        "reject_definition" => Some(WorkflowEffectOperationV1::RejectDefinition),
        "handoff_issue" => Some(WorkflowEffectOperationV1::HandoffIssue),
        "handoff_redeem" => Some(WorkflowEffectOperationV1::HandoffRedeem),
        _ => None,
    }
}

fn workflow_storage_problem(error: &crate::errors::TraceDecayError) -> DaemonInvocationProblem {
    match error {
        crate::errors::TraceDecayError::ResetRequired { authority, .. }
            if authority == "workflow" =>
        {
            DaemonInvocationProblem::ResetRequired
        }
        _ => DaemonInvocationProblem::Unavailable,
    }
}

fn workflow_effect_problem(problem: DaemonInvocationProblem) -> WorkflowEffectProblemV1 {
    match problem {
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            WorkflowEffectProblemV1::NotFoundOrNotAuthorized
        }
        DaemonInvocationProblem::InvalidRequest
        | DaemonInvocationProblem::UnsupportedRevision
        | DaemonInvocationProblem::ResetRequired => WorkflowEffectProblemV1::InvalidRequest,
        DaemonInvocationProblem::Unavailable => WorkflowEffectProblemV1::Conflict,
    }
}

fn workflow_effect_daemon_problem(problem: WorkflowEffectProblemV1) -> DaemonInvocationProblem {
    match problem {
        WorkflowEffectProblemV1::NotFoundOrNotAuthorized => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowEffectProblemV1::InvalidRequest
        | WorkflowEffectProblemV1::Conflict
        | WorkflowEffectProblemV1::TimedOut => DaemonInvocationProblem::InvalidRequest,
    }
}

fn workflow_effect_outcome(
    terminal: &WorkflowEffectTerminalV1,
) -> Result<WorkflowApplicationOutcome, DaemonInvocationProblem> {
    match terminal.outcome() {
        WorkflowEffectOutcomeV1::Problem(WorkflowEffectProblemV1::TimedOut) => {
            let termination = EffectTermination::TimedOut;
            match terminal.identity().operation() {
                WorkflowEffectOperationV1::RegisterDefinition => work_effect::<
                    tracedecay_domain::WorkflowDefinition,
                >(
                    terminal, None, termination
                )
                .map(WorkflowApplicationOutcome::RegisterDefinition)
                .map_err(|_| DaemonInvocationProblem::Unavailable),
                WorkflowEffectOperationV1::ActivateDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::ActivateDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::RetireDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RetireDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::RejectDefinition => {
                    work_effect::<WorkflowDefinitionDisposition>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::RejectDefinition)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::HandoffIssue => {
                    work_effect::<TaskHandoffGrant>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffIssue)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
                WorkflowEffectOperationV1::HandoffRedeem => {
                    work_effect::<TaskHandoffRedeemed>(terminal, None, termination)
                        .map(WorkflowApplicationOutcome::HandoffRedeem)
                        .map_err(|_| DaemonInvocationProblem::Unavailable)
                }
            }
        }
        WorkflowEffectOutcomeV1::Problem(problem) => Err(workflow_effect_daemon_problem(*problem)),
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RegisterDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionActivated(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::ActivateDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRetired(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RetireDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRejected(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::RejectDefinition)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffIssue)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
        WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(result)) => {
            work_effect(
                terminal,
                Some((**result).clone()),
                EffectTermination::Completed,
            )
            .map(WorkflowApplicationOutcome::HandoffRedeem)
            .map_err(|_| DaemonInvocationProblem::Unavailable)
        }
    }
}

fn workflow_coordination_problem(error: WorkflowCoordinationError) -> DaemonInvocationProblem {
    match error {
        WorkflowCoordinationError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        WorkflowCoordinationError::DefinitionNotFound => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowCoordinationError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        WorkflowCoordinationError::InvalidDefinition
        | WorkflowCoordinationError::ImmutableDefinitionConflict
        | WorkflowCoordinationError::IllegalLifecycleTransition
        | WorkflowCoordinationError::LifecycleRevisionConflict => {
            DaemonInvocationProblem::InvalidRequest
        }
    }
}

fn task_handoff_problem(error: TaskHandoffError) -> DaemonInvocationProblem {
    match error {
        TaskHandoffError::AuthorityUnavailable(_) => DaemonInvocationProblem::Unavailable,
        TaskHandoffError::Missing | TaskHandoffError::ScopeMismatch => {
            DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
        TaskHandoffError::InvalidToken
        | TaskHandoffError::InvalidScope
        | TaskHandoffError::Unauthorized
        | TaskHandoffError::InvalidExpiry
        | TaskHandoffError::Conflict
        | TaskHandoffError::Expired
        | TaskHandoffError::Replay => DaemonInvocationProblem::InvalidRequest,
    }
}

/// Maps a verified Work topology failure to the typed application problem the
/// attempt-list read reports. Absence of any Work events is the only
/// non-error state: it names an empty scope, not a failing authority.
fn work_topology_problem(
    error: tracedecay_runtime_core::work_topology::WorkTopologyError,
) -> Result<tracedecay_application::WorkAttemptTopologyStateV1, ApplicationProblem> {
    use tracedecay_runtime_core::work_topology::WorkTopologyError;
    match error {
        WorkTopologyError::EmptyEvents => {
            Ok(tracedecay_application::WorkAttemptTopologyStateV1::Absent)
        }
        WorkTopologyError::Cancelled => Err(ApplicationProblem::Cancelled {
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        WorkTopologyError::BudgetExhausted => Err(ApplicationProblem::TimedOut {
            retry: RetryDirective::AfterDelay,
            legal_actions: Vec::new(),
        }),
        WorkTopologyError::GenerationMismatch => Err(ApplicationProblem::stale(SafeDiagnostic {
            code: "work.topology_generation_superseded".to_owned(),
            message: "The verified Work topology generation was superseded during the read"
                .to_owned(),
        })),
        WorkTopologyError::MixedAuthority
        | WorkTopologyError::NonCanonicalTasks
        | WorkTopologyError::DependencyCycle(_)
        | WorkTopologyError::Contract(_)
        | WorkTopologyError::Corrupt(_)
        | WorkTopologyError::Unavailable(_) => Err(work_topology_unavailable_problem(
            "the verified Work topology could not be served",
        )),
    }
}

fn work_topology_unavailable_problem(message: &str) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "work.topology_unavailable".to_owned(),
        message: message.to_owned(),
    })
}

#[allow(clippy::too_many_arguments)]
fn work_projection_problem(error: WorkProjectionApplicationError) -> ApplicationProblem {
    match error {
        WorkProjectionApplicationError::Admission(problem) => problem,
        WorkProjectionApplicationError::InvalidPageSize => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.invalid_page_size".to_owned(),
                message: "The Work projection page size is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
        },
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::StaleCursor,
        ) => ApplicationProblem::stale(SafeDiagnostic {
            code: "work.stale_cursor".to_owned(),
            message: "The Work projection cursor is stale".to_owned(),
        }),
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::Unavailable,
        ) => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.projection_unavailable".to_owned(),
            message: "The Work projection authority is unavailable".to_owned(),
        }),
        WorkProjectionApplicationError::Port(
            tracedecay_application::WorkProjectionPortError::NotFoundOrNotAuthorized,
        ) => ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_work_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
    };
    let outcome = match work_evidence_packet(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(evidence) => wrap(ApplicationOutcome::Evidence(evidence)),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_work_effect<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, ApplicationProblem>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    let result = match result {
        Ok(result) => result,
        Err(problem) => return application_problem(request_id, problem),
    };
    let outcome = match work_command_effect(
        registered,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    ) {
        Ok(effect) => wrap(effect),
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::WorkApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn work_command_effect<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Work input digest",
        })?
        .to_owned();
    let idempotency_key = IdempotencyKey::new(format!("work.{operation_key}.{suffix}"))?;
    let expected_state = canonical_sha256(&(
        "tracedecay.work.expected-state.v1",
        operation_key,
        &input_digest,
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "Work expected state",
    })?;
    let committed_state =
        canonical_sha256(&("tracedecay.work.committed-state.v1", operation_key, &result)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            },
        )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    let receipt = EffectReceipt {
        operation: use_case,
        request_id,
        actor: registered.actor.clone(),
        scope: context.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: idempotency_key.clone(),
        input_digest,
        expected_state: expected_state.clone(),
        policy_digest: authority.policy.digest.clone(),
        configuration_digest: registered.configuration_digest.clone(),
        catalog_digest: canonical_sha256(&("tracedecay.work.catalog.v1", operation_key)).map_err(
            |_| ApplicationContractError::Inconsistent {
                field: "Work catalog digest",
            },
        )?,
        privacy_digest: canonical_sha256(&(
            "tracedecay.work.privacy.v1",
            context.scope(),
            context.grant().disclosure,
        ))
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "Work privacy digest",
        })?,
        outcome: EffectTermination::Completed,
        committed_state: Some(committed_state),
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        EffectId::new(format!("effect.work.{operation_key}.{suffix}"))?,
        EffectClass::Administrative,
        idempotency_key,
        authority,
        expected_state,
        execution,
        ReconciliationState::Reconciled,
        receipt,
        Some(result),
    )?))
}

pub(super) fn work_request_context(
    registered: &RegisteredWorkRuntime,
    request_id: &str,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, RequestId, UseCaseId), DaemonInvocationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let use_case = UseCaseId::new(use_case).map_err(|_| DaemonInvocationProblem::Unavailable)?;
    let canonical_request_id =
        RequestId::new(request_id).map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        canonical_request_id.clone(),
        deadline,
        cancellation,
    )
    .map_err(|_| DaemonInvocationProblem::NotFoundOrNotAuthorized)?;
    if context.admission_at(observed_at) != RequestAdmission::Admitted
        || !context.allows(&capability, &use_case)
    {
        return Err(DaemonInvocationProblem::NotFoundOrNotAuthorized);
    }
    Ok((context, canonical_request_id, use_case))
}

#[allow(clippy::too_many_arguments)]
fn work_evidence_packet<T>(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    _request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: T,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<EvidencePacket<T>, ApplicationContractError>
where
    T: Serialize,
{
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.work-read-policy.v1",
        &registered.policy_digest,
        &registered.grant.digest,
        operation_key,
        &use_case,
    ))?;
    let policy = PolicyDecisionRef::new(
        format!("policy.daemon.work.{operation_key}.v1"),
        1,
        policy_digest,
        ComponentVersion::new("tracedecay.daemon.work-policy.v1").map_err(|_| {
            ApplicationContractError::Inconsistent {
                field: "Work read policy evaluator",
            }
        })?,
    )?;
    let authority = AuthorityReceipt::from_context(context, policy, observed_at)?;
    let suffix = input_digest.as_str().strip_prefix("sha256:").ok_or(
        ApplicationContractError::Inconsistent {
            field: "Work read input digest",
        },
    )?;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )?;
    Ok(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: vec![EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!("evidence.work.{operation_key}.{suffix}"))?,
            source_kind: "work_projection".to_owned(),
            producer: operation_key.to_owned(),
            scope: context.scope().clone(),
            revision: ComponentVersion::new("tracedecay.work-projection.v1")?,
            horizon: Some(execution.ended_at),
        }],
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(format!("sort.work.{operation_key}.v1"))?,
            1,
            Some(1),
            1,
        )?,
        execution,
        payload: Some(result),
    })
}

#[allow(clippy::too_many_arguments)]
fn work_effect<T>(
    terminal: &WorkflowEffectTerminalV1,
    result: Option<T>,
    termination: EffectTermination,
) -> Result<ApplicationOutcome<T>, ApplicationContractError>
where
    T: Serialize,
{
    let identity = terminal.identity();
    let receipt_context = identity.receipt_context();
    let committed_state = result
        .as_ref()
        .map(|result| {
            canonical_sha256(&(
                "tracedecay.work.committed-state.v1",
                identity.operation().as_str(),
                result,
            ))
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "Work committed state",
            })
        })
        .transpose()?;
    let execution = OperationReceipt {
        started_at: identity.started_at(),
        ended_at: terminal.ended_at(),
        effective_deadline: identity.deadline().clone(),
        cancellation: workflow_effect_terminal_observation(termination, terminal.ended_at()),
        budget: OperationBudgetUsage::default(),
        termination: termination.into(),
    };
    execution.validate()?;
    let receipt = EffectReceipt {
        operation: receipt_context.operation().clone(),
        request_id: identity.request_id().clone(),
        actor: identity.actor().clone(),
        scope: identity.scope().clone(),
        effect_class: EffectClass::Administrative,
        idempotency_key: identity.idempotency_key().clone(),
        input_digest: identity.input_digest().clone(),
        expected_state: receipt_context.expected_state().clone(),
        policy_digest: receipt_context.authority().policy.digest.clone(),
        configuration_digest: receipt_context.configuration_digest().clone(),
        catalog_digest: receipt_context.catalog_digest().clone(),
        privacy_digest: receipt_context.privacy_digest().clone(),
        outcome: termination,
        committed_state,
        external_proof: None,
    };
    Ok(ApplicationOutcome::Effect(EffectResult::new(
        receipt_context.effect_id().clone(),
        EffectClass::Administrative,
        identity.idempotency_key().clone(),
        receipt_context.authority().clone(),
        receipt_context.expected_state().clone(),
        execution,
        ReconciliationState::Reconciled,
        receipt,
        result,
    )?))
}

fn workflow_effect_terminal_observation(
    termination: EffectTermination,
    observed_at: UtcMicros,
) -> Option<CancellationObservation> {
    (termination == EffectTermination::TimedOut).then_some(CancellationObservation {
        stage: CancellationStage::BeforeEffect,
        observed_at,
    })
}

impl DaemonInvocationService {
    pub(super) async fn work_runtime(
        &self,
        project_root: Option<&Path>,
    ) -> Option<RegisteredWorkRuntime> {
        self.project_runtimes.get(project_root?).await
    }
}

#[cfg(test)]
mod workflow_effect_receipt_tests {
    use super::*;

    #[test]
    fn deadline_before_mutation_is_not_labeled_in_flight() {
        let observed_at = UtcMicros(42);
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::TimedOut, observed_at),
            Some(CancellationObservation {
                stage: CancellationStage::BeforeEffect,
                observed_at,
            })
        );
        assert_eq!(
            workflow_effect_terminal_observation(EffectTermination::Completed, observed_at),
            None
        );
    }

    #[test]
    fn workflow_reset_refusal_remains_a_daemon_reset_problem() {
        let error =
            crate::errors::TraceDecayError::reset_required("workflow", "partial workflow schema");
        assert_eq!(
            workflow_storage_problem(&error),
            DaemonInvocationProblem::ResetRequired
        );
    }
}
