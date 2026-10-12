//! The guided Work job: one request names a bounded parallel job with a
//! reviewed synthesis, and a pure planner picks the next existing Work
//! operation from what the Work authorities currently hold.
//!
//! The job keeps no state of its own. Every identity is derived from the job
//! name and a task key, so each call re-observes the graph, placement, and
//! attempt authorities and acts only on what is absent. A committed step is
//! therefore observed, never replayed, and two calls on one job converge.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    AttemptId, CommitId, InitiativeId, MAX_WORK_PRODUCT_TEXT_BYTES, MilestoneId, ProposalId, RefId,
    RunId, TaskId, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkEffectStateV1,
    WorkHierarchyV1, WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1, WorkPlanId,
    WorkProductGraphV1, WorkflowOperationRef, WorkflowOutputName,
};

use crate::{
    AdmittedWorkExecutionV1, ApplicationProblem, ApplicationProblemRecord, EffectReceipt,
    LegalAction, RequestId, RetryDirective, SafeDiagnostic, WorkProductMutationReceiptV1,
    WorkSynthesisAttemptV1,
};

/// Bounds the per-call observation, which re-reads every task's objects.
pub const MAX_WORK_JOB_TASKS: usize = 16;
const MAX_WORK_JOB_SLUG_BYTES: usize = 48;

pub const WORK_JOB_CONFLICT_CODE: &str = "application.work-guided.job-conflict";
pub const WORK_JOB_PROPOSAL_WITHHELD_CODE: &str = "application.work-guided.proposal-withheld";
pub const WORK_JOB_NEEDS_OPERATOR_CODE: &str = "application.work-guided.needs-operator";
const WORK_JOB_INVALID_CODE: &str = "application.work-guided.invalid-job";

macro_rules! work_job_slug {
    ($name:ident, $field:literal) => {
        #[doc = concat!("A job ", $field, ": `[a-z0-9-]`, 1 to 48 bytes. Identities derive from it.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ApplicationProblem> {
                let value = value.into();
                let canonical = !value.is_empty()
                    && value.len() <= MAX_WORK_JOB_SLUG_BYTES
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    });
                if canonical {
                    Ok(Self(value))
                } else {
                    Err(invalid_job(concat!(
                        "A job ",
                        $field,
                        " must be 1 to 48 bytes of lowercase letters, digits, or hyphens."
                    )))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(|problem| serde::de::Error::custom(problem.safe_message()))
            }
        }
    };
}

work_job_slug!(WorkJobNameV1, "name");
work_job_slug!(WorkJobKeyV1, "task key");

/// One guided job: the tasks to run in parallel, where each runs, and the
/// synthesis that reviews them.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdvanceWorkJobRequestV1 {
    pub job: WorkJobNameV1,
    pub title: String,
    pub base: WorkJobBaseV1,
    pub effect_state: WorkEffectStateV1,
    pub tasks: Vec<WorkJobTaskV1>,
    pub synthesis: WorkJobSynthesisV1,
}

/// The source revision every attempt in the job starts from.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobBaseV1 {
    pub reference: Option<RefId>,
    pub commit: CommitId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobTaskV1 {
    pub key: WorkJobKeyV1,
    pub title: String,
    pub instructions: String,
    pub placement: WorkPlacementTargetV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobSynthesisV1 {
    /// The synthesis attempt runs under this task's run and execution snapshot.
    pub task: WorkJobKeyV1,
    pub output_name: WorkflowOutputName,
    pub instructions: String,
}

/// What one call did and where the job now stands.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdvanceWorkJobResultV1 {
    /// The derived identities, so the advanced operations can target them.
    pub job: WorkJobIdentitiesV1,
    /// Only the steps this call executed, in order.
    pub performed: Vec<WorkJobStepRecordV1>,
    pub state: WorkJobStateV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobIdentitiesV1 {
    pub operation: WorkflowOperationRef,
    pub tasks: Vec<WorkJobTaskIdentitiesV1>,
    pub synthesis: WorkAttemptIdentityV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobTaskIdentitiesV1 {
    pub key: WorkJobKeyV1,
    pub proposal_id: ProposalId,
    pub source: WorkAttemptIdentityV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkJobStateV1 {
    /// The call's deadline floor arrived with work left; call again now.
    Advancing,
    /// Nothing can move until providers, routing, or capacity progress.
    Waiting { on: Vec<WorkJobWaitV1> },
    /// The chain stopped at `step`. `problem` is the record the step's own
    /// operation produced, or a guided refusal naming the job-level cause.
    Refused {
        step: WorkJobStepV1,
        problem: Box<ApplicationProblemRecord>,
    },
    Completed {
        sources: Vec<WorkAttemptV1>,
        synthesis: Box<WorkAttemptV1>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "wait", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkJobWaitV1 {
    /// The proposal route abstained, as it does while routing warms.
    ProposalRoute { task: WorkJobKeyV1, reason: String },
    Attempt {
        identity: WorkAttemptIdentityV1,
        state: WorkAttemptStateV1,
    },
    /// The registered topology has no free attempt slot for this task.
    Capacity {
        task: WorkJobKeyV1,
        problem: Box<ApplicationProblemRecord>,
    },
}

/// One existing Work operation the guided job runs. There is deliberately no
/// arm that advances a job, so a step can never recurse into the guided path.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkJobStepV1 {
    CreateTask { task: WorkJobKeyV1 },
    AcceptProposal { task: WorkJobKeyV1 },
    AdmitExecution { task: WorkJobKeyV1 },
    AdmitPlacement { task: WorkJobKeyV1 },
    StartSource { task: WorkJobKeyV1 },
    AwaitSources,
    Synthesize,
    AwaitSynthesis,
    ReleasePlacement { task: WorkJobKeyV1 },
}

impl WorkJobStepV1 {
    /// `guided.<job>.<step>[.<task>]`. The prepare-based steps use it as the
    /// graph command id, so a retried step re-prepares under the same command.
    pub fn request_id(&self, job: &WorkJobNameV1) -> String {
        let (step, task) = match self {
            Self::CreateTask { task } => ("create_task", Some(task)),
            Self::AcceptProposal { task } => ("accept_proposal", Some(task)),
            Self::AdmitExecution { task } => ("admit_execution", Some(task)),
            Self::AdmitPlacement { task } => ("admit_placement", Some(task)),
            Self::StartSource { task } => ("start_source", Some(task)),
            Self::AwaitSources => ("await_sources", None),
            Self::Synthesize => ("synthesize", None),
            Self::AwaitSynthesis => ("await_synthesis", None),
            Self::ReleasePlacement { task } => ("release_placement", Some(task)),
        };
        match task {
            Some(task) => format!("guided.{}.{step}.{}", job.as_str(), task.as_str()),
            None => format!("guided.{}.{step}", job.as_str()),
        }
    }
}

/// A step this call executed, with its operation's own effect receipt.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkJobStepRecordV1 {
    pub step: WorkJobStepV1,
    pub request_id: RequestId,
    pub receipt: EffectReceipt,
    pub result: WorkJobStepResultV1,
}

/// Every arm is an existing Work operation's own output.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum WorkJobStepResultV1 {
    Graph(WorkProductMutationReceiptV1),
    Execution(Box<AdmittedWorkExecutionV1>),
    Placement(WorkPlacementV1),
    Attempt(Box<WorkAttemptV1>),
    Synthesis(Box<WorkSynthesisAttemptV1>),
}

/// One task of a parsed job with every identity it owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkJobTaskPlanV1 {
    pub key: WorkJobKeyV1,
    pub title: String,
    pub instructions: String,
    pub placement: WorkPlacementTargetV1,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub proposal_id: ProposalId,
    pub source: WorkAttemptIdentityV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkJobSynthesisPlanV1 {
    pub task: WorkJobKeyV1,
    pub output_name: WorkflowOutputName,
    pub instructions: String,
    pub identity: WorkAttemptIdentityV1,
}

/// A request parsed at the boundary. Every identity is derived here, once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkJobV1 {
    name: WorkJobNameV1,
    title: String,
    base: WorkJobBaseV1,
    effect_state: WorkEffectStateV1,
    hierarchy: WorkHierarchyV1,
    operation: WorkflowOperationRef,
    tasks: Vec<WorkJobTaskPlanV1>,
    synthesis: WorkJobSynthesisPlanV1,
}

impl WorkJobV1 {
    pub fn parse(request: AdvanceWorkJobRequestV1) -> Result<Self, ApplicationProblem> {
        let AdvanceWorkJobRequestV1 {
            job,
            title,
            base,
            effect_state,
            tasks,
            synthesis,
        } = request;
        if tasks.is_empty() || tasks.len() > MAX_WORK_JOB_TASKS {
            return Err(invalid_job("A job names 1 to 16 tasks."));
        }
        let keys = tasks
            .iter()
            .map(|task| task.key.clone())
            .collect::<BTreeSet<_>>();
        if keys.len() != tasks.len() {
            return Err(invalid_job("Every job task key must be unique."));
        }
        if !keys.contains(&synthesis.task) {
            return Err(invalid_job(
                "The synthesis task must be one of the job's tasks.",
            ));
        }
        let canonical_text = |text: &str| {
            tracedecay_domain::canonical_text::is_canonical_text_within(
                text,
                MAX_WORK_PRODUCT_TEXT_BYTES,
            )
        };
        if !canonical_text(&title) || tasks.iter().any(|task| !canonical_text(&task.title)) {
            return Err(invalid_job(
                "Job and task titles must be canonical text of at most 4096 bytes.",
            ));
        }
        if synthesis.instructions.is_empty()
            || tasks.iter().any(|task| task.instructions.is_empty())
        {
            return Err(invalid_job(
                "Every task and the synthesis need instructions.",
            ));
        }
        let name = job.as_str();
        let hierarchy = WorkHierarchyV1::new(
            identity::<InitiativeId>(format!("initiative.{name}"))?,
            identity::<WorkPlanId>(format!("plan.{name}"))?,
            identity::<MilestoneId>(format!("milestone.{name}"))?,
        );
        let tasks = tasks
            .into_iter()
            .map(|task| {
                let key = task.key.as_str();
                let task_id = identity::<TaskId>(format!("task.{name}.{key}"))?;
                let run_id = identity::<RunId>(format!("run.{name}.{key}"))?;
                let source = attempt_identity(
                    &task_id,
                    &run_id,
                    identity::<AttemptId>(format!("attempt.{name}.{key}.source"))?,
                )?;
                Ok(WorkJobTaskPlanV1 {
                    proposal_id: identity::<ProposalId>(format!("proposal.{name}.{key}"))?,
                    task_id,
                    run_id,
                    source,
                    key: task.key,
                    title: task.title,
                    instructions: task.instructions,
                    placement: task.placement,
                })
            })
            .collect::<Result<Vec<_>, ApplicationProblem>>()?;
        let synthesis_task = tasks
            .iter()
            .find(|task| task.key == synthesis.task)
            .ok_or_else(|| invalid_job("The synthesis task must be one of the job's tasks."))?;
        let synthesis = WorkJobSynthesisPlanV1 {
            identity: attempt_identity(
                &synthesis_task.task_id,
                &synthesis_task.run_id,
                identity::<AttemptId>(format!(
                    "attempt.{name}.{}.synthesis",
                    synthesis.task.as_str()
                ))?,
            )?,
            task: synthesis.task,
            output_name: synthesis.output_name,
            instructions: synthesis.instructions,
        };
        Ok(Self {
            operation: identity::<WorkflowOperationRef>(format!("operation.{name}"))?,
            name: job,
            title,
            base,
            effect_state,
            hierarchy,
            tasks,
            synthesis,
        })
    }

    pub fn name(&self) -> &WorkJobNameV1 {
        &self.name
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn base(&self) -> &WorkJobBaseV1 {
        &self.base
    }

    pub const fn effect_state(&self) -> WorkEffectStateV1 {
        self.effect_state
    }

    pub fn hierarchy(&self) -> &WorkHierarchyV1 {
        &self.hierarchy
    }

    pub fn operation(&self) -> &WorkflowOperationRef {
        &self.operation
    }

    pub fn tasks(&self) -> &[WorkJobTaskPlanV1] {
        &self.tasks
    }

    pub fn task(&self, key: &WorkJobKeyV1) -> Option<&WorkJobTaskPlanV1> {
        self.tasks.iter().find(|task| &task.key == key)
    }

    pub fn synthesis(&self) -> &WorkJobSynthesisPlanV1 {
        &self.synthesis
    }

    pub fn identities(&self) -> WorkJobIdentitiesV1 {
        WorkJobIdentitiesV1 {
            operation: self.operation.clone(),
            tasks: self
                .tasks
                .iter()
                .map(|task| WorkJobTaskIdentitiesV1 {
                    key: task.key.clone(),
                    proposal_id: task.proposal_id.clone(),
                    source: task.source.clone(),
                })
                .collect(),
            synthesis: self.synthesis.identity.clone(),
        }
    }
}

/// Authority state read at the top of every planning turn. A task key absent
/// from a map has no such object yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkJobObservationV1 {
    pub graph: Option<WorkProductGraphV1>,
    pub placements: BTreeMap<WorkJobKeyV1, WorkPlacementV1>,
    pub sources: BTreeMap<WorkJobKeyV1, WorkAttemptV1>,
    pub synthesis: Option<WorkAttemptV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkJobNextV1 {
    Act(WorkJobStepV1),
    Wait(Vec<WorkJobWaitV1>),
    Refuse(WorkJobStepV1, ApplicationProblem),
    Complete {
        sources: Vec<WorkAttemptV1>,
        synthesis: WorkAttemptV1,
    },
}

/// Stage-major: every create, then every acceptance, admission, placement,
/// and start; then wait for sources, synthesize, wait for synthesis, release.
/// Observed objects that contradict the request refuse before anything acts.
pub fn next_work_job_step(job: &WorkJobV1, seen: &WorkJobObservationV1) -> WorkJobNextV1 {
    if let Some((step, problem)) = contradiction(job, seen).or_else(|| stuck(job, seen)) {
        return WorkJobNextV1::Refuse(step, problem);
    }
    let item = |task: &WorkJobTaskPlanV1| {
        seen.graph
            .as_ref()
            .and_then(|graph| graph.item(&task.task_id))
    };
    let created = |task: &WorkJobTaskPlanV1| item(task).is_some();
    let accepted = |task: &WorkJobTaskPlanV1| {
        item(task).is_some_and(|item| item.accepted_proposal().is_some())
    };
    let admitted = |task: &WorkJobTaskPlanV1| {
        item(task).is_some_and(|item| item.execution_admitted_at().is_some())
    };
    let placed = |task: &WorkJobTaskPlanV1| seen.placements.contains_key(&task.key);
    let started = |task: &WorkJobTaskPlanV1| seen.sources.contains_key(&task.key);
    let stages: [(
        fn(WorkJobKeyV1) -> WorkJobStepV1,
        &dyn Fn(&WorkJobTaskPlanV1) -> bool,
    ); 5] = [
        (|task| WorkJobStepV1::CreateTask { task }, &created),
        (|task| WorkJobStepV1::AcceptProposal { task }, &accepted),
        (|task| WorkJobStepV1::AdmitExecution { task }, &admitted),
        (|task| WorkJobStepV1::AdmitPlacement { task }, &placed),
        (|task| WorkJobStepV1::StartSource { task }, &started),
    ];
    for (step, done) in stages {
        if let Some(task) = job.tasks.iter().find(|task| !done(task)) {
            return WorkJobNextV1::Act(step(task.key.clone()));
        }
    }
    let running = seen
        .sources
        .values()
        .filter(|attempt| !attempt.state().is_terminal())
        .map(attempt_wait)
        .collect::<Vec<_>>();
    if !running.is_empty() {
        return WorkJobNextV1::Wait(running);
    }
    let Some(synthesis) = &seen.synthesis else {
        return WorkJobNextV1::Act(WorkJobStepV1::Synthesize);
    };
    if !synthesis.state().is_terminal() {
        return WorkJobNextV1::Wait(vec![attempt_wait(synthesis)]);
    }
    if let Some(task) = job.tasks.iter().find(|task| {
        seen.placements
            .get(&task.key)
            .is_some_and(|placement| placement.state() == WorkPlacementStateV1::Admitted)
    }) {
        return WorkJobNextV1::Act(WorkJobStepV1::ReleasePlacement {
            task: task.key.clone(),
        });
    }
    WorkJobNextV1::Complete {
        sources: job
            .tasks
            .iter()
            .filter_map(|task| seen.sources.get(&task.key).cloned())
            .collect(),
        synthesis: synthesis.clone(),
    }
}

/// The first observed object whose request-owned fields differ from this
/// request. Timestamps are never compared, so re-sending the same job always
/// converges.
fn contradiction(
    job: &WorkJobV1,
    seen: &WorkJobObservationV1,
) -> Option<(WorkJobStepV1, ApplicationProblem)> {
    for task in &job.tasks {
        let key = || task.key.clone();
        if let Some(item) = seen
            .graph
            .as_ref()
            .and_then(|graph| graph.item(&task.task_id))
        {
            if item.title() != task.title || item.hierarchy() != &job.hierarchy {
                return Some((
                    WorkJobStepV1::CreateTask { task: key() },
                    job_conflict(task.task_id.as_str(), "title or hierarchy"),
                ));
            }
            if item
                .accepted_proposal()
                .is_some_and(|accepted| accepted != &task.proposal_id)
            {
                return Some((
                    WorkJobStepV1::AcceptProposal { task: key() },
                    job_conflict(task.task_id.as_str(), "accepted proposal"),
                ));
            }
        }
        if seen
            .placements
            .get(&task.key)
            .is_some_and(|placement| placement.target() != &task.placement)
        {
            return Some((
                WorkJobStepV1::AdmitPlacement { task: key() },
                job_conflict(task.run_id.as_str(), "placement target"),
            ));
        }
        if let Some(source) = seen.sources.get(&task.key) {
            let execution = source.execution();
            if execution.instructions() != task.instructions
                || execution.commit() != &job.base.commit
                || execution.reference() != job.base.reference.as_ref()
                || execution.effect_state() != job.effect_state
            {
                return Some((
                    WorkJobStepV1::StartSource { task: key() },
                    job_conflict(
                        task.source.attempt_id().as_str(),
                        "attempt instructions or base",
                    ),
                ));
            }
        }
    }
    None
}

/// The first job object in a state only an operator can move, through the
/// advanced operations the refusal names.
fn stuck(
    job: &WorkJobV1,
    seen: &WorkJobObservationV1,
) -> Option<(WorkJobStepV1, ApplicationProblem)> {
    for task in &job.tasks {
        if let Some(source) = seen.sources.get(&task.key)
            && needs_operator(source.state())
        {
            return Some((
                WorkJobStepV1::AwaitSources,
                needs_operator_problem(format!(
                    "Source attempt {} is {}; recover it with resume-attempts or retry-attempt, \
                     or run the request as a new job.",
                    task.source.attempt_id().as_str(),
                    attempt_state_name(source.state()),
                )),
            ));
        }
    }
    if let Some(synthesis) = &seen.synthesis
        && needs_operator(synthesis.state())
    {
        return Some((
            WorkJobStepV1::AwaitSynthesis,
            needs_operator_problem(format!(
                "Synthesis attempt {} is {}; recover it with resume-attempts or retry-attempt, \
                 or run the request as a new job.",
                job.synthesis.identity.attempt_id().as_str(),
                attempt_state_name(synthesis.state()),
            )),
        ));
    }
    for task in &job.tasks {
        if seen
            .placements
            .get(&task.key)
            .is_some_and(|placement| placement.state() == WorkPlacementStateV1::Quarantined)
        {
            return Some((
                WorkJobStepV1::ReleasePlacement {
                    task: task.key.clone(),
                },
                needs_operator_problem(format!(
                    "Placement for run {} is quarantined; clean its root and release it with \
                     release-placement.",
                    task.run_id.as_str()
                )),
            ));
        }
    }
    None
}

const fn needs_operator(state: WorkAttemptStateV1) -> bool {
    matches!(
        state,
        WorkAttemptStateV1::Failed
            | WorkAttemptStateV1::TimedOut
            | WorkAttemptStateV1::Cancelled
            | WorkAttemptStateV1::RecoveryRequired
    )
}

const fn attempt_state_name(state: WorkAttemptStateV1) -> &'static str {
    match state {
        WorkAttemptStateV1::Leased => "leased",
        WorkAttemptStateV1::Running => "running",
        WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        WorkAttemptStateV1::CancellationAcknowledged => "cancellation_acknowledged",
        WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        WorkAttemptStateV1::Succeeded => "succeeded",
        WorkAttemptStateV1::Failed => "failed",
        WorkAttemptStateV1::TimedOut => "timed_out",
        WorkAttemptStateV1::Cancelled => "cancelled",
    }
}

fn attempt_wait(attempt: &WorkAttemptV1) -> WorkJobWaitV1 {
    WorkJobWaitV1::Attempt {
        identity: attempt.identity().clone(),
        state: attempt.state(),
    }
}

fn job_conflict(object: &str, field: &str) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: WORK_JOB_CONFLICT_CODE.to_owned(),
            message: format!(
                "{object} already exists with a different {field}; name a new job to run this \
                 request."
            ),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
        detail: None,
    }
}

fn needs_operator_problem(message: String) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: WORK_JOB_NEEDS_OPERATOR_CODE.to_owned(),
            message,
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::Reconcile],
        detail: None,
    }
}

/// The proposal decision for a job task is not `allow`, so the guided path
/// does not accept it on the caller's behalf.
pub fn work_job_proposal_withheld(task: &TaskId, disposition: &str) -> ApplicationProblem {
    ApplicationProblem::conflict(
        WORK_JOB_PROPOSAL_WITHHELD_CODE,
        format!(
            "The proposal for {} was decided {disposition}, not allow; review it with \
             generate-proposal and review-proposal.",
            task.as_str()
        ),
    )
}

/// A synthesis that could cite no source leaves the job to an operator.
pub fn work_job_synthesis_unsynthesized(identity: &WorkAttemptIdentityV1) -> ApplicationProblem {
    needs_operator_problem(format!(
        "Synthesis {} found no citable source; inspect the sources with attempt-status.",
        identity.attempt_id().as_str()
    ))
}

fn invalid_job(message: &'static str) -> ApplicationProblem {
    ApplicationProblem::invalid_request(WORK_JOB_INVALID_CODE, message)
}

fn identity<T>(value: String) -> Result<T, ApplicationProblem>
where
    T: TryFrom<String>,
{
    T::try_from(value).map_err(|_| invalid_job("A derived job identity is not canonical."))
}

fn attempt_identity(
    task_id: &TaskId,
    run_id: &RunId,
    attempt_id: AttemptId,
) -> Result<WorkAttemptIdentityV1, ApplicationProblem> {
    WorkAttemptIdentityV1::new(task_id.clone(), run_id.clone(), attempt_id)
        .map_err(|_| invalid_job("A derived job identity is not canonical."))
}

#[cfg(test)]
mod tests;
