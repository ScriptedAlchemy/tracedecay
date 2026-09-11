//! Work and workflow application daemon invocation handlers.

use super::*;

mod attempt_operations;
mod evidence_retrieval;
mod intelligence;
mod leak_adjudication;
mod outcome;
mod preparation;
mod request_dispatch;
pub(crate) mod workflow_census;
mod workflow_dispatch;
mod workflow_effect_journal;
pub(crate) mod workflow_fan_out;
mod workflow_run_control;

use outcome::{
    complete_work_effect, complete_work_read, offer_work_blocked_interval_receipts,
    work_command_effect, work_effect, work_evidence_packet, work_product_problem,
    work_projection_problem, work_request_context,
};
pub(super) use outcome::{work_background_context, work_blocked_interval_recovery_context};
use tracedecay_domain::git::{GitChangeKindV1, GitStatusEntryV1};
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

/// Permanent owner-publication failure. This is not warming: retrying the
/// same request against this server cannot grow the missing owner.
pub(super) fn runtime_publication_failed_problem(request_id: String) -> DaemonInvocationResponse {
    let problem = ApplicationProblem::execution_failed(
        tracedecay_contracts::ApplicationExecutionFailureClassV1::Permanent,
        SafeDiagnostic {
            code: "application.runtime.owner_failed".to_owned(),
            message: "The project runtime for this operation failed to publish; reopen the project"
                .to_owned(),
        },
    );
    match problem {
        Ok(problem) => application_problem(request_id, problem),
        Err(_) => DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::ApplicationContractViolation,
        ),
    }
}

pub(super) fn missing_registered_owner_problem(
    publication: Option<crate::project_runtime::ProjectRuntimePublicationStateV1>,
    request_id: String,
) -> DaemonInvocationResponse {
    if publication == Some(crate::project_runtime::ProjectRuntimePublicationStateV1::Failed) {
        return runtime_publication_failed_problem(request_id);
    }
    runtime_mounting_problem(request_id)
}

/// Dispatches one Work invocation through the product authority and publishes
/// a Task-family activity pulse only after a mutation committed.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "daemon.service.work.execute", future = true)]
pub async fn execute_work_application(
    registered: RegisteredWorkRuntime,
    attempt_processes: Arc<super::work_attempt_exec::WorkAttemptProcessRegistryV1>,
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
    // Keep the Work dispatch and activity-publication frame out of invocation callers.
    Box::pin(async move {
        let activity_database = registered.database.clone();
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
            tracedecay_session_memory::event_lane::publish(
                &activity_database,
                tracedecay_session_memory::event_lane::ActivityFamilyV1::Task,
                project_root,
                None,
                1,
                work_activity_detail(&response.outcome),
            )
            .await;
        }
        response
    })
    .await
}

fn publish_committed_task_activity_in_background(
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    project_root: PathBuf,
    detail: Option<String>,
) {
    tokio::spawn(async move {
        tracedecay_session_memory::event_lane::publish(
            &database,
            tracedecay_session_memory::event_lane::ActivityFamilyV1::Task,
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
        WorkApplicationInvocationV1::GenerateProposal(_)
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
        | WorkApplicationInvocationV1::ReviewProposal(_)
        | WorkApplicationInvocationV1::AcceptProposal(_)
        | WorkApplicationInvocationV1::AdmitExecution(_)
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

fn observe_placement_target(
    project_root: Option<&std::path::Path>,
    target: &tracedecay_domain::WorkPlacementTargetV1,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkPlacementObservationV1, ApplicationProblem> {
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
    let unique_commits = placement_unique_commits(&repository, &root);
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
        unique_commits,
        readable: true,
        ..unreadable
    })
}

/// Count commits reachable only through this placement's checked-out branch.
/// Snapshot object ids before traversal so a moving ref cannot turn an
/// unmeasured state into a false zero; any failed read remains `None`.
fn placement_unique_commits(
    repository: &tracedecay_runtime_core::git_repository::GitRepositoryAuthority,
    root: &std::path::Path,
) -> Option<u32> {
    let head = repository.head().ok()?;
    let head_commit = head.commit()?.as_str().to_owned();
    let current_reference = head.branch().map(|branch| format!("refs/heads/{branch}"));
    let mut other_tips = repository
        .references()
        .ok()?
        .into_iter()
        .filter(|reference| Some(reference.name.as_str()) != current_reference.as_deref())
        .filter_map(|reference| reference.target.map(|target| target.as_str().to_owned()))
        .collect::<Vec<_>>();
    other_tips.sort_unstable();
    other_tips.dedup();

    let mut owned_arguments = vec!["rev-list".to_owned(), "--count".to_owned(), head_commit];
    if !other_tips.is_empty() {
        owned_arguments.push("--not".to_owned());
        owned_arguments.extend(other_tips);
    }
    let arguments = owned_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let repository_root = repository.worktree_root().unwrap_or(root);
    tracedecay_runtime_core::git::git_capture(repository_root, &arguments)?
        .parse()
        .ok()
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

#[cfg(test)]
mod placement_observation_tests {
    use super::*;
    use tracedecay_domain::{
        RunId, TaskId, WorkPlacementBlockerV1, WorkPlacementIdentityV1, WorkPlacementKindV1,
        WorkPlacementPreflightV1, WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1,
    };

    fn git(root: &std::path::Path, arguments: &[&str]) {
        let output = std::process::Command::new(
            tracedecay_runtime_core::git::try_git_program().expect("git is available"),
        )
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("git runs");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn release(
        target: WorkPlacementTargetV1,
        observation: tracedecay_domain::WorkPlacementObservationV1,
        run: &str,
    ) -> WorkPlacementV1 {
        let preflight = WorkPlacementPreflightV1::evaluate(
            WorkPlacementIdentityV1::new(
                TaskId::new("task.placement-observation").expect("task id"),
                RunId::new(run).expect("run id"),
            ),
            target.clone(),
            observation,
        );
        let admitted =
            WorkPlacementV1::admit(&preflight, None, UtcMicros(200)).expect("placement admits");
        admitted
            .release(observation.removal_blockers(&target), UtcMicros(300))
            .expect("placement releases or quarantines")
    }

    #[test]
    fn release_keeps_only_commits_not_reachable_from_another_ref() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repository_root = sandbox.path().join("repository");
        let placement_root = sandbox.path().join("placement");
        std::fs::create_dir(&repository_root).expect("repository root");
        git(
            &repository_root,
            &["init", "--initial-branch=main", "--quiet"],
        );
        git(
            &repository_root,
            &["config", "user.name", "TraceDecay Test"],
        );
        git(
            &repository_root,
            &["config", "user.email", "test@tracedecay.invalid"],
        );
        std::fs::write(repository_root.join("fixture"), "base\n").expect("base fixture");
        git(&repository_root, &["add", "fixture"]);
        git(&repository_root, &["commit", "--quiet", "-m", "base"]);
        git(
            &repository_root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "placement",
                placement_root.to_str().expect("UTF-8 placement root"),
                "main",
            ],
        );
        let target = WorkPlacementTargetV1::new(
            WorkPlacementKindV1::LinkedWorktree,
            Some(
                placement_root
                    .to_str()
                    .expect("UTF-8 placement root")
                    .to_owned(),
            ),
            false,
            true,
        )
        .expect("linked target");

        let clean = observe_placement_target(None, &target, UtcMicros(100)).expect("observation");
        assert_eq!(clean.unique_commits, Some(0));
        assert_eq!(
            release(target.clone(), clean, "run.clean").state(),
            WorkPlacementStateV1::Released
        );

        std::fs::write(placement_root.join("fixture"), "diverged\n").expect("changed fixture");
        git(&placement_root, &["add", "fixture"]);
        git(
            &placement_root,
            &["commit", "--quiet", "-m", "diverge placement"],
        );
        let diverged =
            observe_placement_target(None, &target, UtcMicros(400)).expect("observation");
        assert_eq!(diverged.unique_commits, Some(1));
        let quarantined = release(target, diverged, "run.diverged");
        assert_eq!(quarantined.state(), WorkPlacementStateV1::Quarantined);
        assert_eq!(
            quarantined.blockers(),
            &std::collections::BTreeSet::from([WorkPlacementBlockerV1::UniqueCommits])
        );
    }
}
