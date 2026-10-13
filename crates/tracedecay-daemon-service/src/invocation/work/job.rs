//! The guided Work job shell.
//!
//! It only observes and executes. Each turn reads the graph, placement, and
//! attempt authorities, asks the pure planner for the next step, and runs that
//! step through [`super::execute_work_application`], the dispatch every
//! transport uses. A step therefore gets its own capability check, receipt,
//! activity pulse, and typed refusal, exactly as a step-by-step caller would.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_contracts::{
    AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand, AdvanceWorkJobRequestV1,
    AdvanceWorkJobResultV1, ApplicationOutcome, ApplicationProblem, ApplicationProblemRecord,
    CancellationContext, Deadline, EffectReceipt, GenerateProposalRequest,
    PrepareWorkProductMutationRequestV1, ReleaseWorkPlacementCommand, RequestContext, RequestId,
    SafeDiagnostic, StartWorkAttemptCommand, WorkAttemptStatusRequestV1, WorkGraphReadV1,
    WorkJobKeyV1, WorkJobNextV1, WorkJobObservationV1, WorkJobStateV1, WorkJobStepRecordV1,
    WorkJobStepResultV1, WorkJobStepV1, WorkJobTaskPlanV1, WorkJobV1, WorkJobWaitV1,
    WorkPlacementReadingV1, WorkPlacementStatusRequestV1, WorkProductChangeDraftV1,
    WorkProductMutationRequestV1, WorkProductSelectionScopeV1, WorkSynthesisAttemptV1,
    next_work_job_step, now_micros, work_job_proposal_withheld, work_job_synthesis_unsynthesized,
};
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationResponse, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1,
};
use tracedecay_domain::{
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanV1, WorkProductGraphV1,
    WorkRouteDecisionV1,
};
use tracedecay_policy::work_loop::WorkProposalDispositionV1;
use tracedecay_tool_catalog::UseCaseId;

use super::super::work_attempt_exec::WorkAttemptProcessRegistryV1;
use super::{RegisteredWorkRuntime, preparation, work_product_problem};

/// ponytail: fixed floor; derive it from measured step latency if jobs grow
/// past `MAX_WORK_JOB_TASKS`.
const YIELD_FLOOR_MICROS: i64 = 5_000_000;
/// Two calls on one job race to one compare-and-swap; the loser re-observes.
const MAX_CONTENTION_RETRIES: u8 = 3;
const CONTENDED_CODES: [&str; 2] = [
    "work.graph_idempotency_conflict",
    "work.graph_version_conflict",
];
const CAPACITY_EXHAUSTED_CODE: &str = "application.work-attempt.capacity-exhausted";

/// Everything a step's own Work invocation needs, borrowed from the call
/// that advances the job.
pub(super) struct WorkJobHost {
    pub(super) registered: RegisteredWorkRuntime,
    pub(super) attempt_processes: Arc<WorkAttemptProcessRegistryV1>,
    pub(super) observability_producer:
        Option<Arc<tracedecay_application::observability::BoundedObservabilityProducerV1>>,
    pub(super) project_root: Option<PathBuf>,
    /// The request's own repository, which every graph step selects.
    pub(super) selection: WorkProductSelectionScopeV1,
    pub(super) deadline: Deadline,
    pub(super) cancellation: CancellationContext,
}

/// Why a step stopped short of a record.
enum Stop {
    Wait(WorkJobWaitV1),
    Problem(ApplicationProblem),
}

impl From<ApplicationProblem> for Stop {
    fn from(problem: ApplicationProblem) -> Self {
        Self::Problem(problem)
    }
}

#[tracing::instrument(name = "daemon.service.work.advance_job", level = "trace", skip_all)]
pub(super) async fn advance_job(
    host: &WorkJobHost,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: AdvanceWorkJobRequestV1,
) -> Result<AdvanceWorkJobResultV1, ApplicationProblem> {
    let job = WorkJobV1::parse(request)?;
    let mut performed = Vec::new();
    let mut contention = 0_u8;
    let state = loop {
        if host.deadline.expires_at.0.saturating_sub(now_micros().0) < YIELD_FLOOR_MICROS {
            break WorkJobStateV1::Advancing;
        }
        let seen = observe(host, context, capability, use_case, &job)?;
        let step = match next_work_job_step(&job, &seen) {
            WorkJobNextV1::Act(step) => step,
            WorkJobNextV1::Wait(on) => break WorkJobStateV1::Waiting { on },
            WorkJobNextV1::Refuse(step, problem) => {
                let problem = problem_record(&step.request_id(job.name()), problem)?;
                break WorkJobStateV1::Refused { step, problem };
            }
            WorkJobNextV1::Complete { sources, synthesis } => {
                break WorkJobStateV1::Completed { sources, synthesis };
            }
        };
        match act(host, &job, &seen, &step).await {
            Ok(record) => performed.push(record),
            Err(Stop::Wait(wait)) => break WorkJobStateV1::Waiting { on: vec![wait] },
            Err(Stop::Problem(problem))
                if contention < MAX_CONTENTION_RETRIES && is_contended(&problem) =>
            {
                contention += 1;
            }
            Err(Stop::Problem(problem)) => {
                let problem = problem_record(&step.request_id(job.name()), problem)?;
                break WorkJobStateV1::Refused { step, problem };
            }
        }
    };
    Ok(AdvanceWorkJobResultV1 {
        job: job.identities(),
        performed,
        state,
    })
}

fn is_contended(problem: &ApplicationProblem) -> bool {
    problem
        .diagnostic()
        .is_some_and(|diagnostic| CONTENDED_CODES.contains(&diagnostic.code.as_str()))
}

fn problem_record(
    request_id: &str,
    problem: ApplicationProblem,
) -> Result<Box<ApplicationProblemRecord>, ApplicationProblem> {
    let request_id = RequestId::new(request_id).map_err(|_| step_unavailable())?;
    ApplicationProblemRecord::from_problem(request_id, problem)
        .map(Box::new)
        .map_err(|_| step_unavailable())
}

/// Reads every object the job owns, directly from the registered services.
fn observe(
    host: &WorkJobHost,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    job: &WorkJobV1,
) -> Result<WorkJobObservationV1, ApplicationProblem> {
    let registered = &host.registered;
    let services = tracedecay_application::work::RegisteredWorkApplicationServicesV1::attach(
        &registered.database,
    )
    .map_err(|_| step_unavailable())?;
    let graph = match preparation::read_repository_graph(
        registered,
        context,
        capability,
        use_case,
        now_micros(),
    )? {
        WorkGraphReadV1::Absent { .. } => None,
        read => Some(
            read.into_current_snapshot()
                .map_err(work_product_problem)?
                .graph()
                .clone(),
        ),
    };
    let mut placements = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for task in job.tasks() {
        let reading = services.placement().status(
            context,
            &WorkPlacementStatusRequestV1 {
                task_id: task.task_id.clone(),
                run_id: task.run_id.clone(),
            },
        )?;
        if let WorkPlacementReadingV1::Placed { placement } = reading {
            placements.insert(task.key.clone(), placement);
        }
        if let Some(attempt) = attempt_status(&services, context, &task.source)? {
            sources.insert(task.key.clone(), attempt);
        }
    }
    let synthesis = attempt_status(&services, context, &job.synthesis().identity)?;
    Ok(WorkJobObservationV1 {
        graph,
        placements,
        sources,
        synthesis,
    })
}

/// The attempt, or `None` when it was never started. Absence and denial share
/// one concealed answer, and every identity read here is the job's own.
fn attempt_status(
    services: &tracedecay_application::work::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
) -> Result<Option<tracedecay_domain::WorkAttemptV1>, ApplicationProblem> {
    match services.attempts().status(
        context,
        &WorkAttemptStatusRequestV1 {
            task_id: identity.task_id().clone(),
            run_id: identity.run_id().clone(),
            attempt_id: identity.attempt_id().clone(),
        },
    ) {
        Ok(attempt) => Ok(Some(attempt)),
        Err(ApplicationProblem::NotFoundOrNotAuthorized { .. }) => Ok(None),
        Err(problem) => Err(problem),
    }
}

/// Runs one step through Work dispatch under the request id
/// `guided.<job>.<step>.<task>`. A prepare-based step makes two calls under
/// that id, the prepare (whose command id it becomes) and the effect.
async fn act(
    host: &WorkJobHost,
    job: &WorkJobV1,
    seen: &WorkJobObservationV1,
    step: &WorkJobStepV1,
) -> Result<WorkJobStepRecordV1, Stop> {
    let request_id = step.request_id(job.name());
    let task = |key: &WorkJobKeyV1| job.task(key).ok_or_else(step_unavailable);
    let (receipt, result) = match step {
        WorkJobStepV1::CreateTask { task: key } => {
            let change = create_change(job, task(key)?, seen.graph.as_ref())?;
            let prepared = prepare(host, &request_id, change).await?;
            let (receipt, mutation) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::MutateGraph(prepared),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::MutateGraph(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            (receipt, WorkJobStepResultV1::Graph(Box::new(mutation)))
        }
        WorkJobStepV1::AcceptProposal { task: key } => {
            let task = task(key)?;
            let generated = read(
                host,
                &request_id,
                WorkApplicationInvocationV1::GenerateProposal(GenerateProposalRequest {
                    selection: host.selection.clone(),
                    task_id: task.task_id.clone(),
                    proposal_id: task.proposal_id.clone(),
                    live_git_evidence: None,
                    occurred_at: now_micros(),
                }),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::GenerateProposal(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            if let WorkRouteDecisionV1::Abstained { reason } = generated.proposal.route() {
                return Err(Stop::Wait(WorkJobWaitV1::ProposalRoute {
                    task: key.clone(),
                    reason: reason.clone(),
                }));
            }
            if generated.decision.disposition != WorkProposalDispositionV1::Allow {
                return Err(work_job_proposal_withheld(
                    &task.task_id,
                    disposition_name(generated.decision.disposition),
                )
                .into());
            }
            let prepared = prepare(
                host,
                &request_id,
                WorkProductChangeDraftV1::DecideProposal {
                    proposal: generated.proposal,
                    disposition: tracedecay_domain::WorkProposalDispositionV1::Accepted,
                },
            )
            .await?;
            let (receipt, mutation) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::MutateGraph(prepared),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::MutateGraph(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            (receipt, WorkJobStepResultV1::Graph(Box::new(mutation)))
        }
        WorkJobStepV1::AdmitExecution { task: key } => {
            let prepared = prepare(
                host,
                &request_id,
                WorkProductChangeDraftV1::AdmitExecution {
                    task_id: task(key)?.task_id.clone(),
                },
            )
            .await?;
            let WorkProductMutationRequestV1::AdmitExecution(admission) = prepared else {
                return Err(step_unavailable().into());
            };
            let (receipt, admitted) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::AdmitExecution(admission),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::AdmitExecution(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            (receipt, WorkJobStepResultV1::Execution(Box::new(admitted)))
        }
        WorkJobStepV1::AdmitPlacement { task: key } => {
            let task = task(key)?;
            let (receipt, placement) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::AdmitPlacement(AdmitWorkPlacementCommand {
                    task_id: task.task_id.clone(),
                    run_id: task.run_id.clone(),
                    target: task.placement.clone(),
                    retention_eligible_at: None,
                    occurred_at: now_micros(),
                }),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::AdmitPlacement(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            (receipt, WorkJobStepResultV1::Placement(placement))
        }
        WorkJobStepV1::StartSource { task: key } => {
            let task = task(key)?;
            let start = start_command(host, job, seen, task, &task.source, &task.instructions)?;
            let (receipt, attempt) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::StartAttempt(start),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::StartAttempt(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await
            .map_err(|stop| capacity_wait(stop, key, &request_id))?;
            (receipt, WorkJobStepResultV1::Attempt(Box::new(attempt)))
        }
        WorkJobStepV1::Synthesize => {
            let synthesis = job.synthesis();
            let task = task(&synthesis.task)?;
            let start = start_command(
                host,
                job,
                seen,
                task,
                &synthesis.identity,
                &synthesis.instructions,
            )?;
            let (receipt, admitted) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::Synthesize(AdmitWorkSynthesisCommand {
                    start,
                    output_name: synthesis.output_name.clone(),
                    sources: job.tasks().iter().map(|task| task.source.clone()).collect(),
                }),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::Synthesize(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await
            .map_err(|stop| capacity_wait(stop, &synthesis.task, &request_id))?;
            if let WorkSynthesisAttemptV1::Unsynthesized { .. } = admitted {
                return Err(work_job_synthesis_unsynthesized(&synthesis.identity).into());
            }
            (receipt, WorkJobStepResultV1::Synthesis(Box::new(admitted)))
        }
        WorkJobStepV1::ReleasePlacement { task: key } => {
            let task = task(key)?;
            let placement = seen.placements.get(key).ok_or_else(step_unavailable)?;
            let (receipt, released) = effect(
                host,
                &request_id,
                WorkApplicationInvocationV1::ReleasePlacement(ReleaseWorkPlacementCommand {
                    task_id: task.task_id.clone(),
                    run_id: task.run_id.clone(),
                    expected_authority_version: placement.authority_version(),
                    occurred_at: now_micros(),
                }),
                |outcome| match outcome {
                    WorkApplicationOutcomeV1::ReleasePlacement(outcome) => Some(outcome),
                    _ => None,
                },
            )
            .await?;
            (receipt, WorkJobStepResultV1::Placement(released))
        }
        WorkJobStepV1::AwaitSources | WorkJobStepV1::AwaitSynthesis => {
            return Err(step_unavailable().into());
        }
    };
    Ok(WorkJobStepRecordV1 {
        step: step.clone(),
        request_id: RequestId::new(request_id).map_err(|_| step_unavailable())?,
        receipt,
        result,
    })
}

/// The first task of a job also creates the job's initiative, plan, and
/// milestone; later tasks join them.
fn create_change(
    job: &WorkJobV1,
    task: &WorkJobTaskPlanV1,
    graph: Option<&WorkProductGraphV1>,
) -> Result<WorkProductChangeDraftV1, ApplicationProblem> {
    let hierarchy = job.hierarchy();
    let now = now_micros();
    let invalid = |_| {
        ApplicationProblem::invalid_request(
            "application.work-guided.invalid-job",
            "The job's titles do not form a valid Work task.",
        )
    };
    let item = Box::new(
        WorkItemV1::new(WorkItemInputV1 {
            task_id: task.task_id.clone(),
            hierarchy: hierarchy.clone(),
            title: task.title.clone(),
            dependencies: BTreeSet::default(),
            informational_relations: BTreeSet::default(),
            causal_candidates: BTreeSet::default(),
            acceptance_criteria: Vec::new(),
            effort: 1,
            scheduled_at: None,
            deadline: None,
            created_at: now,
            updated_at: now,
        })
        .map_err(invalid)?,
    );
    let created = graph.is_some_and(|graph| {
        graph
            .initiatives()
            .iter()
            .any(|initiative| initiative.id() == hierarchy.initiative_id())
    });
    if created {
        return Ok(WorkProductChangeDraftV1::AddTask { item });
    }
    let title = || job.title().to_owned();
    Ok(WorkProductChangeDraftV1::CreateTask {
        initiative: WorkInitiativeV1::new(hierarchy.initiative_id().clone(), title(), now)
            .map_err(invalid)?,
        plan: WorkPlanV1::new(
            hierarchy.plan_id().clone(),
            hierarchy.initiative_id().clone(),
            title(),
            now,
        )
        .map_err(invalid)?,
        milestone: WorkMilestoneV1::new(
            hierarchy.milestone_id().clone(),
            hierarchy.plan_id().clone(),
            title(),
            now,
        )
        .map_err(invalid)?,
        item,
    })
}

/// A start for `identity` under `task`'s run, carrying the execution snapshot
/// re-derived from the task's accepted proposal and recorded admission, so a
/// later call starts with the bytes admission returned.
fn start_command(
    host: &WorkJobHost,
    job: &WorkJobV1,
    seen: &WorkJobObservationV1,
    task: &WorkJobTaskPlanV1,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
    instructions: &str,
) -> Result<StartWorkAttemptCommand, ApplicationProblem> {
    let graph = seen.graph.as_ref().ok_or_else(step_unavailable)?;
    let admitted_at = graph
        .item(&task.task_id)
        .and_then(WorkItemV1::execution_admitted_at)
        .ok_or_else(step_unavailable)?;
    let execution_snapshot = preparation::admitted_execution_snapshot(
        &host.registered,
        graph,
        &task.task_id,
        admitted_at,
    )?;
    // An attempt runs in the root its run's placement admitted; a placement
    // without a root keeps the project root.
    let placement = seen
        .placements
        .get(&task.key)
        .ok_or_else(step_unavailable)?;
    let worktree_root = match placement.target().root() {
        Some(root) => root.to_owned(),
        None => host
            .project_root
            .as_ref()
            .map(|root| root.to_string_lossy().into_owned())
            .ok_or_else(step_unavailable)?,
    };
    Ok(StartWorkAttemptCommand {
        task_id: identity.task_id().clone(),
        run_id: identity.run_id().clone(),
        attempt_id: identity.attempt_id().clone(),
        operation: job.operation().clone(),
        execution_snapshot,
        worktree_root,
        reference: job.base().reference.clone(),
        commit: job.base().commit.clone(),
        instructions: instructions.to_owned(),
        effect_state: job.effect_state(),
        occurred_at: now_micros(),
    })
}

fn capacity_wait(stop: Stop, task: &WorkJobKeyV1, request_id: &str) -> Stop {
    match stop {
        Stop::Problem(problem)
            if problem
                .diagnostic()
                .is_some_and(|diagnostic| diagnostic.code == CAPACITY_EXHAUSTED_CODE) =>
        {
            match problem_record(request_id, problem) {
                Ok(problem) => Stop::Wait(WorkJobWaitV1::Capacity {
                    task: task.clone(),
                    problem,
                }),
                Err(problem) => Stop::Problem(problem),
            }
        }
        stop => stop,
    }
}

async fn prepare(
    host: &WorkJobHost,
    request_id: &str,
    change: WorkProductChangeDraftV1,
) -> Result<WorkProductMutationRequestV1, Stop> {
    read(
        host,
        request_id,
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: host.selection.clone(),
            change,
            causation_event_id: None,
            evidence: Vec::new(),
        }),
        |outcome| match outcome {
            WorkApplicationOutcomeV1::PrepareGraphMutation(outcome) => Some(outcome),
            _ => None,
        },
    )
    .await
}

async fn read<T>(
    host: &WorkJobHost,
    request_id: &str,
    invocation: WorkApplicationInvocationV1,
    select: fn(WorkApplicationOutcomeV1) -> Option<ApplicationOutcome<T>>,
) -> Result<T, Stop> {
    match select(invoke(host, request_id, invocation).await?) {
        Some(ApplicationOutcome::Evidence(packet)) => {
            packet.payload.ok_or_else(|| step_unavailable().into())
        }
        _ => Err(step_unavailable().into()),
    }
}

async fn effect<T>(
    host: &WorkJobHost,
    request_id: &str,
    invocation: WorkApplicationInvocationV1,
    select: fn(WorkApplicationOutcomeV1) -> Option<ApplicationOutcome<T>>,
) -> Result<(EffectReceipt, T), Stop> {
    match select(invoke(host, request_id, invocation).await?) {
        Some(ApplicationOutcome::Effect(effect)) => match effect.payload {
            Some(payload) => Ok((effect.receipt, payload)),
            None => Err(step_unavailable().into()),
        },
        _ => Err(step_unavailable().into()),
    }
}

async fn invoke(
    host: &WorkJobHost,
    request_id: &str,
    invocation: WorkApplicationInvocationV1,
) -> Result<WorkApplicationOutcomeV1, ApplicationProblem> {
    let response = dispatch_step(host, request_id.to_owned(), invocation).await;
    match response.outcome {
        DaemonInvocationOutcome::WorkApplication { outcome, .. } => Ok(outcome),
        DaemonInvocationOutcome::ApplicationProblem { problem } => Err(problem),
        _ => Err(step_unavailable()),
    }
}

/// The recursion into Work dispatch has a nameable boxed type, which breaks
/// the cycle between this future and the dispatch future that contains it.
/// `WorkJobStepV1` has no arm that advances a job, so it recurses once.
fn dispatch_step(
    host: &WorkJobHost,
    request_id: String,
    invocation: WorkApplicationInvocationV1,
) -> Pin<Box<dyn Future<Output = DaemonInvocationResponse> + Send>> {
    Box::pin(super::execute_work_application(
        host.registered.clone(),
        Arc::clone(&host.attempt_processes),
        host.observability_producer.clone(),
        host.project_root.clone(),
        request_id,
        invocation,
        now_micros(),
        host.deadline.clone(),
        host.cancellation.clone(),
    ))
}

const fn disposition_name(disposition: WorkProposalDispositionV1) -> &'static str {
    match disposition {
        WorkProposalDispositionV1::Allow => "allow",
        WorkProposalDispositionV1::Deny => "deny",
        WorkProposalDispositionV1::Abstain => "abstain",
        WorkProposalDispositionV1::Indeterminate => "indeterminate",
    }
}

fn step_unavailable() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-guided.step-unavailable".to_owned(),
        message: "A guided Work step returned no result its operation defines.".to_owned(),
    })
}
