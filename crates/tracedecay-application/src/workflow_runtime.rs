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
    WorkCommandId, WorkEffectStateV1, WorkExecutionBudgetV1, WorkLeaseFenceV1,
    WorkProviderBackendV1, WorkProviderRouteV1, WorkflowDefinitionId, WorkflowDefinitionV1,
    WorkflowOperationRef, WorkflowPlacementReceiptV1, WorkflowStepId, canonical_sha256,
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
    pub topology_digest: ManifestDigest,
    pub provider_registry_digest: ManifestDigest,
    pub worktree_placement: WorktreePlacementModeV1,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    pub deadline: UtcMicros,
    #[schemars(range(min = 1))]
    pub cancellation_generation: u64,
    pub budget: WorkExecutionBudgetV1,
    pub effect_state: WorkEffectStateV1,
}

impl WorkflowProviderAdmissionV1 {
    pub fn placement(
        &self,
        run_id: RunId,
        step_id: WorkflowStepId,
    ) -> Result<WorkflowPlacementReceiptV1, WorkflowFanOutRuntimeError> {
        WorkflowPlacementReceiptV1::new(
            run_id,
            step_id,
            self.route.clone(),
            self.backend,
            self.model.clone(),
            self.configuration_digest.clone(),
            self.topology_digest.clone(),
            self.provider_registry_digest.clone(),
            self.worktree_placement.clone(),
        )
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
    }
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

fn command_id(operation: &str, suffix: &str) -> Result<WorkCommandId, WorkflowFanOutRuntimeError> {
    WorkCommandId::new(format!("workflow-child-{operation}:{suffix}"))
        .map_err(|_| WorkflowFanOutRuntimeError::InvalidPlan)
}
