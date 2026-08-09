//! Work and workflow application daemon invocation handlers.

use super::*;

mod attempt_operations;
mod evidence_retrieval;
mod intelligence;
mod leak_adjudication;
mod outcome;
mod preparation;
mod request_dispatch;
pub(in crate::daemon::service::invocation) mod workflow_census;
mod workflow_dispatch;
mod workflow_effect_journal;
pub(in crate::daemon::service::invocation) mod workflow_fan_out;
mod workflow_run_control;

use outcome::{
    complete_work_effect, complete_work_read, offer_work_blocked_interval_receipts,
    work_command_effect, work_effect, work_evidence_packet, work_product_problem,
    work_projection_problem, work_request_context, work_topology_problem,
    work_topology_unavailable_problem,
};
pub(super) use outcome::{work_background_context, work_blocked_interval_recovery_context};
pub(super) use workflow_dispatch::execute_workflow_application;
use workflow_fan_out::reconcile_active_workflow_fan_out;

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

/// Retryable state for an admitted project whose runtime is still mounting.
pub(super) fn runtime_mounting_problem(request_id: String) -> DaemonInvocationResponse {
    application_problem(
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.surface.unavailable".to_owned(),
            message: "The project runtime for this operation is still mounting".to_owned(),
        }),
    )
}

/// Dispatches one Work invocation through the product authority and publishes
/// a Task-family activity pulse only after a mutation committed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
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
    let activity_database = Arc::clone(&registered.database);
    let activity_root = project_root.clone();
    let mutates = work_invocation_mutates(&request);
    let response = request_dispatch::dispatch_work_application(
        registered,
        attempt_processes,
        observability_producer,
        project_root,
        request_id,
        request,
        observed_at,
        deadline,
        cancellation,
    )
    .await;
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

fn publish_committed_task_activity_in_background(
    database: Arc<crate::global_db::RegisteredGlobalDb>,
    project_root: PathBuf,
    detail: Option<String>,
) {
    tokio::spawn(async move {
        crate::application::event_lane::publish(
            &database,
            crate::application::event_lane::ActivityFamilyV1::Task,
            &project_root,
            None,
            1,
            detail.as_deref(),
        )
        .await;
    });
}

const fn work_invocation_mutates(request: &WorkApplicationInvocationV1) -> bool {
    match request {
        WorkApplicationInvocationV1::Snapshot(_)
        | WorkApplicationInvocationV1::Delta(_)
        | WorkApplicationInvocationV1::GenerateProposal(_)
        | WorkApplicationInvocationV1::AttemptStatus(_)
        | WorkApplicationInvocationV1::ListAttempts(_)
        | WorkApplicationInvocationV1::ExecutionHistory(_)
        | WorkApplicationInvocationV1::HydrateArtifacts(_)
        | WorkApplicationInvocationV1::RetrieveEvidence(_)
        | WorkApplicationInvocationV1::Views(_)
        | WorkApplicationInvocationV1::Experience(_)
        | WorkApplicationInvocationV1::CompareProposal(_)
        | WorkApplicationInvocationV1::PrepareGraphMutation(_)
        | WorkApplicationInvocationV1::Topology(_)
        | WorkApplicationInvocationV1::TopologyMetrics(_)
        | WorkApplicationInvocationV1::PrepareDuplicateAdjudication(_)
        | WorkApplicationInvocationV1::RunControl(_)
        | WorkApplicationInvocationV1::PlacementPreflight(_)
        | WorkApplicationInvocationV1::PlacementStatus(_) => false,
        WorkApplicationInvocationV1::Create(_)
        | WorkApplicationInvocationV1::ReplanDependencies(_)
        | WorkApplicationInvocationV1::ReviewProposal(_)
        | WorkApplicationInvocationV1::AcceptProposal(_)
        | WorkApplicationInvocationV1::AdmitExecution(_)
        | WorkApplicationInvocationV1::AcceptTask(_)
        | WorkApplicationInvocationV1::StartAttempt(_)
        | WorkApplicationInvocationV1::Synthesize(_)
        | WorkApplicationInvocationV1::CancelAttempt(_)
        | WorkApplicationInvocationV1::ResumeAttempts(_)
        | WorkApplicationInvocationV1::RetryAttempt(_)
        | WorkApplicationInvocationV1::MutateGraph(_)
        | WorkApplicationInvocationV1::AdjudicateDuplicate(_)
        | WorkApplicationInvocationV1::AdjudicateLeak(_)
        | WorkApplicationInvocationV1::PauseRun(_)
        | WorkApplicationInvocationV1::ResumeRun(_)
        | WorkApplicationInvocationV1::AdmitPlacement(_)
        | WorkApplicationInvocationV1::ReleasePlacement(_) => true,
    }
}

/// Observes one placement target through the native Git authority.
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
        active_holder: false,
        network_required: false,
        observed_at,
    };
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
