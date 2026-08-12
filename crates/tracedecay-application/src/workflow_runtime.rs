//! Durable workflow planning contracts over canonical Work attempts.
//!
//! This module deliberately owns no child scheduler or provider adapter. The
//! daemon uses the immutable plan below to create, admit, lease, dispatch, and
//! settle every child through the canonical Work runtime and queue.

use std::collections::BTreeSet;
use std::fmt::{self, Display};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::WorktreePlacementModeV1;
use tracedecay_domain::{
    AttemptId, CommitId, ManifestDigest, RefId, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkCommandId, WorkEffectStateV1, WorkExecutionSnapshot, WorkInitiativeV1, WorkItemV1,
    WorkLeaseFenceV1, WorkMilestoneV1, WorkPlanV1, WorkProposalV1, WorkflowDefinition,
    WorkflowDefinitionId, WorkflowOperationRef, WorkflowPlacementReceipt, WorkflowStepId,
    canonical_sha256,
};

use crate::context::CancellationContext;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionIdentity {
    pub definition_id: WorkflowDefinitionId,
    pub definition_version: u64,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowExecutionFence {
    pub attempt_id: AttemptId,
    pub lease: WorkLeaseFenceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutInput {
    pub instructions: String,
    pub input_digest: ManifestDigest,
    pub initiative: WorkInitiativeV1,
    pub plan: WorkPlanV1,
    pub milestone: WorkMilestoneV1,
    pub item: WorkItemV1,
    pub proposal: WorkProposalV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowProviderAdmission {
    pub execution_snapshot: WorkExecutionSnapshot,
    pub topology_digest: ManifestDigest,
    pub provider_registry_digest: ManifestDigest,
    pub worktree_placement: WorktreePlacementModeV1,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    #[schemars(range(min = 1))]
    pub cancellation_generation: u64,
    pub effect_state: WorkEffectStateV1,
}

impl WorkflowProviderAdmission {
    pub fn placement(
        &self,
        run_id: RunId,
        step_id: WorkflowStepId,
    ) -> Result<WorkflowPlacementReceipt, WorkflowFanOutRuntimeError> {
        WorkflowPlacementReceipt::new(
            run_id,
            step_id,
            self.execution_snapshot.route().clone(),
            self.execution_snapshot.backend(),
            self.execution_snapshot.model().to_owned(),
            self.execution_snapshot.effective_behavior_digest().clone(),
            self.topology_digest.clone(),
            self.provider_registry_digest.clone(),
            self.worktree_placement.clone(),
        )
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum WorkflowFailurePolicy {
    FailFast,
    Collect,
    RequireAtLeast {
        #[schemars(range(min = 1))]
        successes: u32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutRequest {
    pub definition: WorkflowDefinition,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub fence: WorkflowExecutionFence,
    pub admitted_at: UtcMicros,
    pub cancellation: CancellationContext,
    #[schemars(range(min = 1))]
    pub max_parallel: u32,
    pub failure_policy: WorkflowFailurePolicy,
    pub provider: WorkflowProviderAdmission,
    pub inputs: Vec<WorkflowFanOutInput>,
}

/// Caller input required to durably plan the entry fan-out of a workflow run.
/// Daemon-owned registration and topology digests are resolved at admission.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutStartV1 {
    pub fence: WorkflowExecutionFence,
    #[schemars(range(min = 1))]
    pub max_parallel: u32,
    pub failure_policy: WorkflowFailurePolicy,
    pub execution_snapshot: WorkExecutionSnapshot,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    pub effect_state: WorkEffectStateV1,
    pub inputs: Vec<WorkflowFanOutInput>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlannedChild {
    pub ordinal: u32,
    pub task_id: TaskId,
    pub attempt_identity: WorkAttemptIdentityV1,
    pub create_command_id: WorkCommandId,
    pub proposal_command_id: WorkCommandId,
    pub admit_command_id: WorkCommandId,
    pub evidence_command_id: WorkCommandId,
    pub input: WorkflowFanOutInput,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutPlan {
    pub identity: WorkflowExecutionIdentity,
    pub operation: WorkflowOperationRef,
    pub admitted_at: UtcMicros,
    pub max_parallel: u32,
    pub failure_policy: WorkflowFailurePolicy,
    pub plan_digest: ManifestDigest,
    pub children: Vec<WorkflowPlannedChild>,
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
    ResetRequired,
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
            Self::ResetRequired => {
                formatter.write_str("workflow store is incompatible and requires reset")
            }
            Self::ChildUnavailable(message) => {
                write!(formatter, "workflow child execution unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for WorkflowFanOutRuntimeError {}

pub fn prepare_workflow_fan_out(
    request: &WorkflowFanOutRequest,
) -> Result<WorkflowFanOutPlan, WorkflowFanOutRuntimeError> {
    request
        .definition
        .validate()
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
    if request
        .provider
        .execution_snapshot
        .effective_behavior_digest()
        != request.definition.pinned_configuration_digest()
        || request.admitted_at.0 <= 0
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
    if let WorkflowFailurePolicy::RequireAtLeast { successes } = request.failure_policy
        && (successes == 0
            || usize::try_from(successes).map_or(true, |value| value > request.inputs.len()))
    {
        return Err(WorkflowFanOutRuntimeError::InvalidFailurePolicy);
    }

    let mut inputs = request.inputs.clone();
    inputs.sort_by(|left, right| left.item.task_id().cmp(right.item.task_id()));
    let mut identities = BTreeSet::new();
    for input in &inputs {
        let task_id = input.item.task_id();
        if input.instructions.is_empty()
            || input.instructions.trim() != input.instructions
            || input.instructions.len() > 512
            || input.instructions.chars().any(char::is_control)
            || input.proposal.task_id() != task_id
            || input.plan.initiative_id() != input.initiative.id()
            || input.milestone.plan_id() != input.plan.id()
            || input.item.hierarchy().initiative_id() != input.initiative.id()
            || input.item.hierarchy().plan_id() != input.plan.id()
            || input.item.hierarchy().milestone_id() != input.milestone.id()
        {
            return Err(WorkflowFanOutRuntimeError::InvalidChildIdentity(
                task_id.as_str().to_owned(),
            ));
        }
        if !identities.insert(task_id.clone()) {
            return Err(WorkflowFanOutRuntimeError::DuplicateChildIdentity(
                task_id.as_str().to_owned(),
            ));
        }
    }

    let identity = WorkflowExecutionIdentity {
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
            &plan_digest,
            ordinal,
            &input,
        ))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)?;
        let suffix = child_digest.as_str();
        let task_id = input.item.task_id().clone();
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
        children.push(WorkflowPlannedChild {
            ordinal,
            task_id,
            attempt_identity,
            create_command_id: command_id("create", suffix)?,
            proposal_command_id: command_id("proposal", suffix)?,
            admit_command_id: command_id("admit", suffix)?,
            evidence_command_id: command_id("evidence", suffix)?,
            input,
        });
    }
    Ok(WorkflowFanOutPlan {
        identity,
        operation: step.operation.clone(),
        admitted_at: request.admitted_at,
        max_parallel: request.max_parallel,
        failure_policy: request.failure_policy,
        plan_digest,
        children,
    })
}

pub fn durable_workflow_fan_out_plan(
    plan: &WorkflowFanOutPlan,
    provider: &WorkflowProviderAdmission,
    authority: tracedecay_domain::WorkAuthority,
) -> Result<tracedecay_domain::WorkflowFanOutPlanV1, WorkflowFanOutRuntimeError> {
    let maximum_parallel = u16::try_from(plan.max_parallel)
        .ok()
        .and_then(std::num::NonZeroU16::new)
        .ok_or(WorkflowFanOutRuntimeError::InvalidParallelism)?;
    let failure_policy = match plan.failure_policy {
        WorkflowFailurePolicy::FailFast => {
            tracedecay_domain::WorkflowFanOutFailurePolicyV1::FailFast
        }
        WorkflowFailurePolicy::Collect => tracedecay_domain::WorkflowFanOutFailurePolicyV1::Collect,
        WorkflowFailurePolicy::RequireAtLeast { successes } => {
            let successes = u16::try_from(successes)
                .ok()
                .and_then(std::num::NonZeroU16::new)
                .ok_or(WorkflowFanOutRuntimeError::InvalidFailurePolicy)?;
            tracedecay_domain::WorkflowFanOutFailurePolicyV1::RequireAtLeast { successes }
        }
    };
    Ok(tracedecay_domain::WorkflowFanOutPlanV1 {
        authority,
        step_id: plan.identity.step_id.clone(),
        operation: plan.operation.clone(),
        plan_digest: plan.plan_digest.clone(),
        admitted_at: plan.admitted_at,
        maximum_parallel,
        failure_policy,
        execution_snapshot: provider.execution_snapshot.clone(),
        reference: provider.reference.clone(),
        commit: provider.commit.clone(),
        effect_state: provider.effect_state,
        children: plan
            .children
            .iter()
            .map(|child| tracedecay_domain::WorkflowFanOutChildPlanV1 {
                task_id: child.task_id.clone(),
                attempt_identity: child.attempt_identity.clone(),
                create_command_id: child.create_command_id.clone(),
                proposal_command_id: child.proposal_command_id.clone(),
                admit_command_id: child.admit_command_id.clone(),
                initiative: child.input.initiative.clone(),
                plan: child.input.plan.clone(),
                milestone: child.input.milestone.clone(),
                item: child.input.item.clone(),
                proposal: child.input.proposal.clone(),
                instructions: child.input.instructions.clone(),
            })
            .collect(),
    })
}

fn command_id(operation: &str, suffix: &str) -> Result<WorkCommandId, WorkflowFanOutRuntimeError> {
    WorkCommandId::new(format!("workflow-child-{operation}:{suffix}"))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
}
