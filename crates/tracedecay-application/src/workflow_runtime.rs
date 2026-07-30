//! Fan-out execution and deterministic synthesis over canonical Work adapters.
//!
//! This module owns orchestration contracts only. Implementations of the
//! authority and child ports must delegate to the canonical Work runtime and
//! its existing automation authority.

/// Production adapter hook still required before a multi-step workflow journey
/// can run without test doubles. The canonical Work runtime owner must implement
/// these ports; this crate must not scaffold a second scheduler or store.
pub const MISSING_CANONICAL_WORK_ADAPTER_HOOK: &str = "canonical Work runtime must implement WorkflowExecutionAuthorityPort, WorkflowChildExecutionPort, and WorkflowSynthesisPort for WorkflowFanOutRuntimeService";

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    AttemptId, ManifestDigest, RunId, TaskId, WorkCommandId, WorkLeaseFenceV1,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef, WorkflowStepId,
    canonical_sha256,
};

use crate::context::CancellationContext;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionIdentityV1 {
    pub definition_id: WorkflowDefinitionId,
    pub definition_version: u64,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionFenceV1 {
    pub attempt_id: AttemptId,
    pub lease: WorkLeaseFenceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutInputV1 {
    pub identity: String,
    pub input_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutRequestV1 {
    pub definition: WorkflowDefinitionV1,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub fence: WorkflowExecutionFenceV1,
    pub cancellation: CancellationContext,
    pub inputs: Vec<WorkflowFanOutInputV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildExecutionRequestV1 {
    pub identity: WorkflowExecutionIdentityV1,
    pub fence: WorkflowExecutionFenceV1,
    pub cancellation: CancellationContext,
    pub task_id: TaskId,
    pub create_command_id: WorkCommandId,
    pub operation: WorkflowOperationRef,
    pub input: WorkflowFanOutInputV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildExecutionBatchV1 {
    pub max_parallel: u32,
    pub cancellation: CancellationContext,
    pub children: Vec<WorkflowChildExecutionRequestV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkflowChildExecutionOutcomeV1 {
    Succeeded {
        output_digest: ManifestDigest,
    },
    Failed {
        evidence_digest: ManifestDigest,
    },
    Interrupted {
        checkpoint_digest: Option<ManifestDigest>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildExecutionResultV1 {
    pub task_id: TaskId,
    pub outcome: WorkflowChildExecutionOutcomeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildRecordV1 {
    pub task_id: TaskId,
    pub input: WorkflowFanOutInputV1,
    pub outcome: WorkflowChildExecutionOutcomeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutCheckpointV1 {
    pub plan_digest: ManifestDigest,
    pub children: Vec<WorkflowChildRecordV1>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryDirectiveV1 {
    #[default]
    ResumeIncomplete,
    RestartAll,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSynthesisChildV1 {
    pub task_id: TaskId,
    pub input_identity: String,
    pub output_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSynthesisRequestV1 {
    pub identity: WorkflowExecutionIdentityV1,
    pub fence: WorkflowExecutionFenceV1,
    /// Deterministic idempotency identity derived from the immutable plan.
    pub synthesis_command_id: WorkCommandId,
    pub children: Vec<WorkflowSynthesisChildV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "truth", rename_all = "snake_case")]
pub enum WorkflowSynthesisTruthV1 {
    Complete {
        output_digest: ManifestDigest,
    },
    Partial {
        output_digest: ManifestDigest,
        failed_children: Vec<TaskId>,
    },
    Failed {
        failed_children: Vec<TaskId>,
        synthesis_error: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowExecutionTruthV1 {
    Synthesized(WorkflowSynthesisTruthV1),
    Interrupted {
        checkpoint: WorkflowFanOutCheckpointV1,
        directive: WorkflowRecoveryDirectiveV1,
    },
    Cancelled {
        cancellation: CancellationContext,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowExecutionAdmissionV1 {
    Execute,
    Recover {
        checkpoint: WorkflowFanOutCheckpointV1,
        directive: WorkflowRecoveryDirectiveV1,
    },
    Replay(WorkflowExecutionTruthV1),
    StaleFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowExecutionAuthorityError {
    Conflict,
    Unavailable(String),
}

/// Atomic authority boundary implemented by the canonical Work owner.
///
/// `begin` fences one plan and returns replay only for the same plan digest.
/// `checkpoint` and `complete` compare the complete attempt/lease fence.
pub trait WorkflowExecutionAuthorityPort: Send + Sync {
    fn begin(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        plan_digest: &ManifestDigest,
    ) -> Result<WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError>;

    fn checkpoint(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        checkpoint: &WorkflowFanOutCheckpointV1,
    ) -> Result<(), WorkflowExecutionAuthorityError>;

    fn complete(
        &self,
        identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        truth: &WorkflowExecutionTruthV1,
    ) -> Result<(), WorkflowExecutionAuthorityError>;
}

/// Executes deterministic children through canonical Work commands/tasks.
///
/// The adapter must enforce `max_parallel` while dispatching the batch. This
/// application service deliberately does not own a scheduler or executor.
pub trait WorkflowChildExecutionPort: Send + Sync {
    fn execute_bounded(
        &self,
        batch: &WorkflowChildExecutionBatchV1,
    ) -> Result<Vec<WorkflowChildExecutionResultV1>, WorkflowFanOutRuntimeError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowSynthesisError {
    Failed(String),
    Unavailable(String),
}

pub trait WorkflowSynthesisPort: Send + Sync {
    fn synthesize(
        &self,
        request: &WorkflowSynthesisRequestV1,
    ) -> Result<ManifestDigest, WorkflowSynthesisError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowFanOutRuntimeError {
    StepNotFound,
    StepIsNotFanOut,
    EmptyFanOut,
    FanOutLimitExceeded { limit: usize, actual: usize },
    InvalidChildIdentity(String),
    DuplicateChildIdentity(String),
    InvalidChildResults,
    InvalidPlan,
    StaleFence,
    AuthorityUnavailable(String),
    ChildUnavailable(String),
}

impl Display for WorkflowFanOutRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StepNotFound => formatter.write_str("workflow step was not found"),
            Self::StepIsNotFanOut => formatter.write_str("workflow step is not fan-out"),
            Self::EmptyFanOut => formatter.write_str("workflow fan-out must not be empty"),
            Self::FanOutLimitExceeded { limit, actual } => {
                write!(formatter, "workflow fan-out {actual} exceeds limit {limit}")
            }
            Self::InvalidChildIdentity(identity) => {
                write!(formatter, "workflow child identity is invalid: {identity}")
            }
            Self::DuplicateChildIdentity(identity) => {
                write!(
                    formatter,
                    "workflow child identity is duplicated: {identity}"
                )
            }
            Self::InvalidChildResults => {
                formatter.write_str("workflow child results do not match the admitted batch")
            }
            Self::InvalidPlan => formatter.write_str("workflow fan-out plan is invalid"),
            Self::StaleFence => formatter.write_str("workflow execution fence is stale"),
            Self::AuthorityUnavailable(message) => {
                write!(
                    formatter,
                    "workflow execution authority unavailable: {message}"
                )
            }
            Self::ChildUnavailable(message) => {
                write!(formatter, "workflow child execution unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for WorkflowFanOutRuntimeError {}

pub struct WorkflowFanOutRuntimeService<A, W, S> {
    authority: A,
    work: W,
    synthesis: S,
}

impl<A, W, S> WorkflowFanOutRuntimeService<A, W, S>
where
    A: WorkflowExecutionAuthorityPort,
    W: WorkflowChildExecutionPort,
    S: WorkflowSynthesisPort,
{
    pub const fn new(authority: A, work: W, synthesis: S) -> Self {
        Self {
            authority,
            work,
            synthesis,
        }
    }

    pub fn execute(
        &self,
        request: WorkflowFanOutRequestV1,
    ) -> Result<WorkflowExecutionTruthV1, WorkflowFanOutRuntimeError> {
        let (identity, operation, max_parallel, plan_digest, children) = prepare_plan(&request)?;
        let admission = self
            .authority
            .begin(&identity, &request.fence, &plan_digest)
            .map_err(authority_error)?;
        let (mut records, directive) = match admission {
            WorkflowExecutionAdmissionV1::Execute => (
                BTreeMap::new(),
                WorkflowRecoveryDirectiveV1::ResumeIncomplete,
            ),
            WorkflowExecutionAdmissionV1::Recover {
                checkpoint,
                directive,
            } => {
                if checkpoint.plan_digest != plan_digest {
                    return Err(WorkflowFanOutRuntimeError::InvalidPlan);
                }
                let records = match directive {
                    WorkflowRecoveryDirectiveV1::ResumeIncomplete => {
                        let mut recovered = BTreeMap::new();
                        for record in checkpoint.children {
                            validate_checkpoint_record(&children, &record)?;
                            if matches!(
                                record.outcome,
                                WorkflowChildExecutionOutcomeV1::Interrupted { .. }
                            ) {
                                continue;
                            }
                            if recovered.insert(record.task_id.clone(), record).is_some() {
                                return Err(WorkflowFanOutRuntimeError::InvalidPlan);
                            }
                        }
                        recovered
                    }
                    WorkflowRecoveryDirectiveV1::RestartAll => BTreeMap::new(),
                };
                (records, directive)
            }
            WorkflowExecutionAdmissionV1::Replay(truth) => return Ok(truth),
            WorkflowExecutionAdmissionV1::StaleFence => {
                return Err(WorkflowFanOutRuntimeError::StaleFence);
            }
        };

        let planned_ids = children
            .iter()
            .map(|child| child.task_id.clone())
            .collect::<BTreeSet<_>>();
        if planned_ids.len() != children.len() {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        }
        if records.keys().any(|task_id| !planned_ids.contains(task_id)) {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        }

        if request.cancellation.is_cancelled() {
            let truth = WorkflowExecutionTruthV1::Cancelled {
                cancellation: request.cancellation.clone(),
            };
            self.authority
                .complete(&identity, &request.fence, &truth)
                .map_err(authority_error)?;
            return Ok(truth);
        }

        let pending = children
            .into_iter()
            .filter(|child| !records.contains_key(&child.task_id))
            .collect::<Vec<_>>();
        let pending_ids = pending
            .iter()
            .map(|child| child.task_id.clone())
            .collect::<BTreeSet<_>>();
        let batch = WorkflowChildExecutionBatchV1 {
            max_parallel,
            cancellation: request.cancellation.clone(),
            children: pending
                .iter()
                .map(|child| WorkflowChildExecutionRequestV1 {
                    identity: identity.clone(),
                    fence: request.fence.clone(),
                    cancellation: request.cancellation.clone(),
                    task_id: child.task_id.clone(),
                    create_command_id: child.create_command_id.clone(),
                    operation: operation.clone(),
                    input: child.input.clone(),
                })
                .collect(),
        };
        let mut results = if batch.children.is_empty() {
            BTreeMap::new()
        } else {
            let returned = self.work.execute_bounded(&batch)?;
            if returned.len() != batch.children.len() {
                return Err(WorkflowFanOutRuntimeError::InvalidChildResults);
            }
            let mut by_task = BTreeMap::new();
            for result in returned {
                if !pending_ids.contains(&result.task_id)
                    || by_task.insert(result.task_id, result.outcome).is_some()
                {
                    return Err(WorkflowFanOutRuntimeError::InvalidChildResults);
                }
            }
            by_task
        };
        let mut interrupted = false;
        for child in pending {
            let outcome = results
                .remove(&child.task_id)
                .ok_or(WorkflowFanOutRuntimeError::InvalidChildResults)?;
            let record = WorkflowChildRecordV1 {
                task_id: child.task_id.clone(),
                input: child.input,
                outcome: outcome.clone(),
            };
            records.insert(child.task_id, record);
            interrupted |= matches!(outcome, WorkflowChildExecutionOutcomeV1::Interrupted { .. });
            let checkpoint = canonical_checkpoint(plan_digest.clone(), records.clone());
            self.authority
                .checkpoint(&identity, &request.fence, &checkpoint)
                .map_err(authority_error)?;
        }
        if !results.is_empty() {
            return Err(WorkflowFanOutRuntimeError::InvalidChildResults);
        }
        if interrupted {
            let checkpoint = canonical_checkpoint(plan_digest.clone(), records);
            return Ok(WorkflowExecutionTruthV1::Interrupted {
                checkpoint,
                directive: match directive {
                    WorkflowRecoveryDirectiveV1::RestartAll => {
                        WorkflowRecoveryDirectiveV1::RestartAll
                    }
                    WorkflowRecoveryDirectiveV1::ResumeIncomplete => {
                        WorkflowRecoveryDirectiveV1::ResumeIncomplete
                    }
                },
            });
        }

        let synthesis_command_id = synthesis_command_id(&identity, &plan_digest)?;
        let checkpoint = canonical_checkpoint(plan_digest, records);
        let truth = synthesize_truth(
            &self.synthesis,
            &identity,
            &request.fence,
            &synthesis_command_id,
            &checkpoint,
        )?;
        self.authority
            .complete(&identity, &request.fence, &truth)
            .map_err(authority_error)?;
        Ok(truth)
    }
}

#[derive(Clone)]
struct PlannedChild {
    task_id: TaskId,
    create_command_id: WorkCommandId,
    input: WorkflowFanOutInputV1,
}

fn prepare_plan(
    request: &WorkflowFanOutRequestV1,
) -> Result<
    (
        WorkflowExecutionIdentityV1,
        WorkflowOperationRef,
        u32,
        ManifestDigest,
        Vec<PlannedChild>,
    ),
    WorkflowFanOutRuntimeError,
> {
    request
        .definition
        .validate()
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    let step = request
        .definition
        .steps()
        .iter()
        .find(|step| step.step_id == request.step_id)
        .ok_or(WorkflowFanOutRuntimeError::StepNotFound)?;
    let fan_out = step
        .fan_out
        .ok_or(WorkflowFanOutRuntimeError::StepIsNotFanOut)?;
    if request.inputs.is_empty() {
        return Err(WorkflowFanOutRuntimeError::EmptyFanOut);
    }
    let limit = usize::try_from(fan_out.max_parallel)
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    if request.inputs.len() > limit {
        return Err(WorkflowFanOutRuntimeError::FanOutLimitExceeded {
            limit,
            actual: request.inputs.len(),
        });
    }

    let mut inputs = request.inputs.clone();
    inputs.sort_by(|left, right| left.identity.cmp(&right.identity));
    let mut identities = BTreeSet::new();
    for input in &inputs {
        if input.identity.is_empty()
            || input.identity.trim() != input.identity
            || input.identity.len() > 512
            || input.identity.chars().any(char::is_control)
        {
            return Err(WorkflowFanOutRuntimeError::InvalidChildIdentity(
                input.identity.clone(),
            ));
        }
        if !identities.insert(input.identity.clone()) {
            return Err(WorkflowFanOutRuntimeError::DuplicateChildIdentity(
                input.identity.clone(),
            ));
        }
    }

    let identity = WorkflowExecutionIdentityV1 {
        definition_id: request.definition.definition_id().clone(),
        definition_version: request.definition.definition_version(),
        run_id: request.run_id.clone(),
        step_id: request.step_id.clone(),
    };
    let plan_digest = canonical_sha256(&(
        "tracedecay.application.workflow-fan-out-plan.v1",
        &identity,
        &request.definition,
        &inputs,
    ))
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    let mut children = Vec::with_capacity(inputs.len());
    for input in inputs {
        let child_digest = canonical_sha256(&(
            "tracedecay.application.workflow-child.v1",
            &identity,
            &input.identity,
        ))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let task_id = TaskId::new(format!("workflow-child:{}", child_digest.as_str()))
            .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let create_command_id =
            WorkCommandId::new(format!("workflow-child-create:{}", child_digest.as_str()))
                .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        children.push(PlannedChild {
            task_id,
            create_command_id,
            input,
        });
    }
    Ok((
        identity,
        step.operation.clone(),
        fan_out.max_parallel,
        plan_digest,
        children,
    ))
}

fn validate_checkpoint_record(
    children: &[PlannedChild],
    record: &WorkflowChildRecordV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    let planned = children
        .iter()
        .find(|child| child.task_id == record.task_id)
        .ok_or(WorkflowFanOutRuntimeError::InvalidPlan)?;
    if record.input.identity != planned.input.identity
        || record.input.input_digest != planned.input.input_digest
    {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    Ok(())
}

fn canonical_checkpoint(
    plan_digest: ManifestDigest,
    records: BTreeMap<TaskId, WorkflowChildRecordV1>,
) -> WorkflowFanOutCheckpointV1 {
    WorkflowFanOutCheckpointV1 {
        plan_digest,
        children: records.into_values().collect(),
    }
}

fn synthesis_command_id(
    identity: &WorkflowExecutionIdentityV1,
    plan_digest: &ManifestDigest,
) -> Result<WorkCommandId, WorkflowFanOutRuntimeError> {
    let digest = canonical_sha256(&(
        "tracedecay.application.workflow-synthesis.v1",
        identity,
        plan_digest,
    ))
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    WorkCommandId::new(format!("workflow-synthesis:{}", digest.as_str()))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
}

fn synthesize_truth<S>(
    synthesis: &S,
    identity: &WorkflowExecutionIdentityV1,
    fence: &WorkflowExecutionFenceV1,
    synthesis_command_id: &WorkCommandId,
    checkpoint: &WorkflowFanOutCheckpointV1,
) -> Result<WorkflowExecutionTruthV1, WorkflowFanOutRuntimeError>
where
    S: WorkflowSynthesisPort,
{
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    for child in &checkpoint.children {
        match &child.outcome {
            WorkflowChildExecutionOutcomeV1::Succeeded { output_digest } => {
                succeeded.push(WorkflowSynthesisChildV1 {
                    task_id: child.task_id.clone(),
                    input_identity: child.input.identity.clone(),
                    output_digest: output_digest.clone(),
                });
            }
            WorkflowChildExecutionOutcomeV1::Failed { .. } => {
                failed.push(child.task_id.clone());
            }
            WorkflowChildExecutionOutcomeV1::Interrupted { .. } => {
                return Err(WorkflowFanOutRuntimeError::InvalidPlan);
            }
        }
    }
    succeeded.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    failed.sort();

    let synthesis_truth = if succeeded.is_empty() {
        WorkflowSynthesisTruthV1::Failed {
            failed_children: failed,
            synthesis_error: None,
        }
    } else {
        match synthesis.synthesize(&WorkflowSynthesisRequestV1 {
            identity: identity.clone(),
            fence: fence.clone(),
            synthesis_command_id: synthesis_command_id.clone(),
            children: succeeded,
        }) {
            Ok(output_digest) if failed.is_empty() => {
                WorkflowSynthesisTruthV1::Complete { output_digest }
            }
            Ok(output_digest) => WorkflowSynthesisTruthV1::Partial {
                output_digest,
                failed_children: failed,
            },
            Err(error) => WorkflowSynthesisTruthV1::Failed {
                failed_children: failed,
                synthesis_error: Some(match error {
                    WorkflowSynthesisError::Failed(message)
                    | WorkflowSynthesisError::Unavailable(message) => message,
                }),
            },
        }
    };
    Ok(WorkflowExecutionTruthV1::Synthesized(synthesis_truth))
}

fn authority_error(error: WorkflowExecutionAuthorityError) -> WorkflowFanOutRuntimeError {
    match error {
        WorkflowExecutionAuthorityError::Conflict => WorkflowFanOutRuntimeError::StaleFence,
        WorkflowExecutionAuthorityError::Unavailable(message) => {
            WorkflowFanOutRuntimeError::AuthorityUnavailable(message)
        }
    }
}
