//! Durable workflow planning contracts over canonical Work attempts.
//!
//! This module deliberately owns no child scheduler or provider adapter. The
//! daemon uses the immutable plan below to create, admit, lease, dispatch, and
//! settle every child through the canonical Work runtime and queue.

use std::collections::BTreeSet;
use std::fmt::{self, Display};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    AttemptId, CommitId, ManifestDigest, RefId, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkAttemptV1, WorkCommandId, WorkEffectStateV1, WorkExecutionBudgetV1, WorkLeaseFenceV1,
    WorkProviderBackendV1, WorkProviderRouteV1, WorkTerminalEvidenceV1, WorkflowDefinitionId,
    WorkflowDefinitionV1, WorkflowOperationRef, WorkflowStepId, canonical_sha256,
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
pub struct WorkflowProviderAdmissionV1 {
    pub route: WorkProviderRouteV1,
    pub backend: WorkProviderBackendV1,
    pub model: String,
    pub configuration_digest: ManifestDigest,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    pub deadline: UtcMicros,
    #[schemars(range(min = 1))]
    pub cancellation_generation: u64,
    pub budget: WorkExecutionBudgetV1,
    pub effect_state: WorkEffectStateV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum WorkflowFailurePolicyV1 {
    FailFast,
    Collect,
    RequireAtLeast {
        #[schemars(range(min = 1))]
        successes: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutRequestV1 {
    pub definition: WorkflowDefinitionV1,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub fence: WorkflowExecutionFenceV1,
    pub admitted_at: UtcMicros,
    pub cancellation: CancellationContext,
    #[schemars(range(min = 1))]
    pub max_parallel: u32,
    pub failure_policy: WorkflowFailurePolicyV1,
    pub provider: WorkflowProviderAdmissionV1,
    pub inputs: Vec<WorkflowFanOutInputV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildReceiptV1 {
    pub observation_digest: ManifestDigest,
    pub terminal_receipt_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowChildRecordV1 {
    pub task_id: TaskId,
    pub attempt_identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub receipt: Option<WorkflowChildReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutCheckpointV1 {
    pub plan_digest: ManifestDigest,
    pub children: Vec<WorkflowChildRecordV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkflowExecutionTruthV1 {
    Completed {
        checkpoint: WorkflowFanOutCheckpointV1,
    },
    Failed {
        checkpoint: WorkflowFanOutCheckpointV1,
    },
    Cancelled {
        checkpoint: WorkflowFanOutCheckpointV1,
        cancellation: CancellationContext,
    },
}

impl WorkflowExecutionTruthV1 {
    pub const fn checkpoint(&self) -> &WorkflowFanOutCheckpointV1 {
        match self {
            Self::Completed { checkpoint }
            | Self::Failed { checkpoint }
            | Self::Cancelled { checkpoint, .. } => checkpoint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowExecutionAdmissionV1 {
    Execute,
    Recover {
        checkpoint: WorkflowFanOutCheckpointV1,
    },
    Replay(WorkflowExecutionTruthV1),
    PlanConflict,
    StaleLease,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowExecutionAuthorityError {
    Conflict,
    Unavailable(String),
}

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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlannedChildV1 {
    pub ordinal: u32,
    pub task_id: TaskId,
    pub attempt_identity: WorkAttemptIdentityV1,
    pub create_command_id: WorkCommandId,
    pub proposal_command_id: WorkCommandId,
    pub admit_command_id: WorkCommandId,
    pub evidence_command_id: WorkCommandId,
    pub proposal_id: tracedecay_domain::ProposalId,
    pub proposal_digest: ManifestDigest,
    pub input: WorkflowFanOutInputV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutPlanV1 {
    pub identity: WorkflowExecutionIdentityV1,
    pub operation: WorkflowOperationRef,
    pub max_parallel: u32,
    pub failure_policy: WorkflowFailurePolicyV1,
    pub plan_digest: ManifestDigest,
    pub children: Vec<WorkflowPlannedChildV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowFanOutRuntimeError {
    StepNotFound,
    StepIsNotFanOut,
    EmptyFanOut,
    FanOutLimitExceeded { limit: usize, actual: usize },
    InvalidParallelism,
    InvalidFailurePolicy,
    InvalidChildIdentity(String),
    DuplicateChildIdentity(String),
    InvalidPlan,
    PlanConflict,
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
                write!(formatter, "workflow fan-out {actual} exceeds width {limit}")
            }
            Self::InvalidParallelism => formatter.write_str("workflow max parallelism is invalid"),
            Self::InvalidFailurePolicy => formatter.write_str("workflow failure policy is invalid"),
            Self::InvalidChildIdentity(identity) => {
                write!(formatter, "workflow child identity is invalid: {identity}")
            }
            Self::DuplicateChildIdentity(identity) => {
                write!(
                    formatter,
                    "workflow child identity is duplicated: {identity}"
                )
            }
            Self::InvalidPlan => formatter.write_str("workflow fan-out plan is invalid"),
            Self::PlanConflict => {
                formatter.write_str("workflow run identity was reused for a different plan")
            }
            Self::StaleFence => formatter.write_str("workflow execution lease is stale"),
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

pub fn prepare_workflow_fan_out(
    request: &WorkflowFanOutRequestV1,
) -> Result<WorkflowFanOutPlanV1, WorkflowFanOutRuntimeError> {
    request
        .definition
        .validate()
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    if request.provider.configuration_digest != *request.definition.pinned_configuration_digest()
        || request.provider.model.is_empty()
        || request.provider.model.trim() != request.provider.model
        || request.admitted_at.0 <= 0
        || request.provider.deadline.0 <= 0
        || request.provider.cancellation_generation == 0
    {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
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
    let width =
        usize::try_from(fan_out.max_width).map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    if request.inputs.len() > width {
        return Err(WorkflowFanOutRuntimeError::FanOutLimitExceeded {
            limit: width,
            actual: request.inputs.len(),
        });
    }
    if request.max_parallel == 0
        || usize::try_from(request.max_parallel).map_or(true, |value| value > request.inputs.len())
    {
        return Err(WorkflowFanOutRuntimeError::InvalidParallelism);
    }
    if let WorkflowFailurePolicyV1::RequireAtLeast { successes } = request.failure_policy
        && (successes == 0
            || usize::try_from(successes).map_or(true, |value| value > request.inputs.len()))
    {
        return Err(WorkflowFanOutRuntimeError::InvalidFailurePolicy);
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
        "tracedecay.application.workflow-fan-out-plan.v2",
        &identity,
        &request.definition,
        request.admitted_at,
        request.max_parallel,
        request.failure_policy,
        &request.provider,
        &inputs,
    ))
    .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    let mut children = Vec::with_capacity(inputs.len());
    for (ordinal, input) in inputs.into_iter().enumerate() {
        let ordinal =
            u32::try_from(ordinal).map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let child_digest = canonical_sha256(&(
            "tracedecay.application.workflow-child.v3",
            &identity,
            ordinal,
            &input.identity,
        ))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let suffix = child_digest.as_str();
        let task_id = TaskId::new(format!("workflow-child:{suffix}"))
            .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let attempt_digest = canonical_sha256(&(
            "tracedecay.application.workflow-child-attempt.v1",
            &identity,
            &plan_digest,
            ordinal,
            &input,
        ))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let attempt_identity = WorkAttemptIdentityV1::new(
            task_id.clone(),
            identity.run_id.clone(),
            AttemptId::new(format!("workflow-work-attempt:{}", attempt_digest.as_str()))
                .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
        )
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        children.push(WorkflowPlannedChildV1 {
            ordinal,
            task_id,
            attempt_identity,
            create_command_id: command_id("create", suffix)?,
            proposal_command_id: command_id("proposal", suffix)?,
            admit_command_id: command_id("admit", suffix)?,
            evidence_command_id: command_id("evidence", suffix)?,
            proposal_id: tracedecay_domain::ProposalId::new(format!(
                "workflow-child-proposal:{suffix}"
            ))
            .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
            proposal_digest: canonical_sha256(&(
                "tracedecay.application.workflow-child-proposal.v1",
                &child_digest,
                &input.input_digest,
                &request.provider,
            ))
            .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?,
            input,
        });
    }
    Ok(WorkflowFanOutPlanV1 {
        identity,
        operation: step.operation.clone(),
        max_parallel: request.max_parallel,
        failure_policy: request.failure_policy,
        plan_digest,
        children,
    })
}

pub fn validate_workflow_checkpoint(
    plan: &WorkflowFanOutPlanV1,
    checkpoint: &WorkflowFanOutCheckpointV1,
) -> Result<(), WorkflowFanOutRuntimeError> {
    if checkpoint.plan_digest != plan.plan_digest || checkpoint.children.len() > plan.children.len()
    {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let mut seen = BTreeSet::new();
    let mut attempts = BTreeSet::new();
    let mut leases = BTreeSet::new();
    for record in &checkpoint.children {
        let planned = plan
            .children
            .iter()
            .find(|child| child.task_id == record.task_id)
            .ok_or(WorkflowFanOutRuntimeError::InvalidPlan)?;
        if record.attempt_identity != planned.attempt_identity
            || record.task_id != *record.attempt_identity.task_id()
            || !seen.insert(record.task_id.clone())
            || !attempts.insert(record.attempt_identity.clone())
            || !leases.insert(record.lease.lease_id().clone())
        {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        }
    }
    Ok(())
}

pub fn workflow_checkpoint(
    plan_digest: ManifestDigest,
    mut children: Vec<WorkflowChildRecordV1>,
) -> WorkflowFanOutCheckpointV1 {
    children.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    WorkflowFanOutCheckpointV1 {
        plan_digest,
        children,
    }
}

pub fn workflow_truth(
    policy: WorkflowFailurePolicyV1,
    checkpoint: WorkflowFanOutCheckpointV1,
    attempts: &[WorkAttemptV1],
) -> Result<WorkflowExecutionTruthV1, WorkflowFanOutRuntimeError> {
    if attempts.len() != checkpoint.children.len() {
        return Err(WorkflowFanOutRuntimeError::InvalidPlan);
    }
    let mut succeeded = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for attempt in attempts {
        if !seen.insert(attempt.identity().clone()) {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        }
        let Some(record) = checkpoint
            .children
            .iter()
            .find(|record| record.attempt_identity == *attempt.identity())
        else {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        };
        if record.task_id != *attempt.identity().task_id() || record.receipt.is_none() {
            return Err(WorkflowFanOutRuntimeError::InvalidPlan);
        }
        match attempt.terminal() {
            Some(WorkTerminalEvidenceV1::Succeeded { .. }) => {
                succeeded.insert(attempt.identity().clone());
            }
            Some(
                WorkTerminalEvidenceV1::Failed { .. }
                | WorkTerminalEvidenceV1::TimedOut { .. }
                | WorkTerminalEvidenceV1::Cancelled { .. },
            ) => {}
            None => return Err(WorkflowFanOutRuntimeError::InvalidPlan),
        }
    }
    let successes = succeeded.len();
    let complete = match policy {
        WorkflowFailurePolicyV1::FailFast | WorkflowFailurePolicyV1::Collect => {
            successes == checkpoint.children.len()
        }
        WorkflowFailurePolicyV1::RequireAtLeast {
            successes: required,
        } => usize::try_from(required).is_ok_and(|required| successes >= required),
    };
    if complete {
        Ok(WorkflowExecutionTruthV1::Completed { checkpoint })
    } else {
        Ok(WorkflowExecutionTruthV1::Failed { checkpoint })
    }
}

fn command_id(operation: &str, suffix: &str) -> Result<WorkCommandId, WorkflowFanOutRuntimeError> {
    WorkCommandId::new(format!("workflow-child-{operation}:{suffix}"))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
}
