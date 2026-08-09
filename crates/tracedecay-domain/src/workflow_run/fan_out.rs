use std::collections::BTreeSet;
use std::num::NonZeroU16;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    CommitId, ManifestDigest, ProposalId, RefId, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkAuthority, WorkCommandId, WorkEffectStateV1, WorkExecutionSnapshot, WorkflowDefinition,
    WorkflowOperationRef, WorkflowStepId,
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
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
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
            || self
                .children
                .iter()
                .any(|child| child.attempt_identity.task_id() != &child.task_id)
        {
            return Err(WorkflowRunStateError::InvalidDefinition);
        }
        Ok(())
    }
}
