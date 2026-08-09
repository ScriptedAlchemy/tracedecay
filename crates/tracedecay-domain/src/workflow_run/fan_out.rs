use std::collections::BTreeSet;
use std::num::NonZeroU16;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CommitId, ManifestDigest, RefId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority,
    WorkCommandId, WorkEffectStateV1, WorkExecutionSnapshot, WorkInitiativeV1, WorkItemV1,
    WorkMilestoneV1, WorkPlanV1, WorkProposalV1, WorkflowDefinition, WorkflowOperationRef,
    WorkflowStepId,
};

use super::WorkflowRunStateError;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum WorkflowFanOutFailurePolicyV1 {
    FailFast,
    Collect,
    RequireAtLeast { successes: NonZeroU16 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutChildPlanV1 {
    pub task_id: TaskId,
    pub attempt_identity: WorkAttemptIdentityV1,
    pub create_command_id: WorkCommandId,
    pub proposal_command_id: WorkCommandId,
    pub admit_command_id: WorkCommandId,
    pub initiative: WorkInitiativeV1,
    pub plan: WorkPlanV1,
    pub milestone: WorkMilestoneV1,
    pub item: WorkItemV1,
    pub proposal: WorkProposalV1,
    pub instructions: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutPlanV1 {
    /// Exact Work authority that admitted this plan. Recovery may execute the
    /// plan only from a runtime with this byte-identical authority.
    pub authority: WorkAuthority,
    pub step_id: WorkflowStepId,
    pub operation: WorkflowOperationRef,
    pub plan_digest: ManifestDigest,
    pub admitted_at: UtcMicros,
    pub maximum_parallel: NonZeroU16,
    pub failure_policy: WorkflowFanOutFailurePolicyV1,
    pub execution_snapshot: WorkExecutionSnapshot,
    pub reference: Option<RefId>,
    pub commit: CommitId,
    pub effect_state: WorkEffectStateV1,
    pub children: Vec<WorkflowFanOutChildPlanV1>,
}

impl WorkflowFanOutPlanV1 {
    pub(super) fn validate(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowRunStateError> {
        let step = definition
            .steps()
            .iter()
            .find(|step| step.step_id == self.step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)?;
        let width = step
            .fan_out
            .ok_or(WorkflowRunStateError::InvalidDefinition)?
            .max_width as usize;
        if self.authority.project_id() != definition.project_id() {
            return Err(WorkflowRunStateError::InvalidDefinition);
        }
        let identities = self
            .children
            .iter()
            .map(|child| &child.attempt_identity)
            .collect::<BTreeSet<_>>();
        if self.children.is_empty()
            || self.children.len() > width
            || identities.len() != self.children.len()
            || usize::from(self.maximum_parallel.get()) > self.children.len()
            || self.children.iter().any(|child| {
                child.attempt_identity.task_id() != &child.task_id
                    || child.item.task_id() != &child.task_id
                    || child.proposal.task_id() != &child.task_id
                    || child.plan.initiative_id() != child.initiative.id()
                    || child.milestone.plan_id() != child.plan.id()
                    || child.item.hierarchy().initiative_id() != child.initiative.id()
                    || child.item.hierarchy().plan_id() != child.plan.id()
                    || child.item.hierarchy().milestone_id() != child.milestone.id()
            })
        {
            return Err(WorkflowRunStateError::InvalidDefinition);
        }
        Ok(())
    }
}
