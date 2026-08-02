//! Event-journaled Workflow run state over immutable definitions.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ManifestDigest, RunId, UtcMicros, WorkArtifactRefV1, WorkCommandId, WorkflowDefinitionV1,
    WorkflowOutputName, WorkflowOutputReferenceV1, WorkflowStepId,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowRunStateError {
    #[error("workflow run history is empty")]
    EmptyHistory,
    #[error("workflow run event sequence is not contiguous")]
    NonContiguousSequence,
    #[error("workflow run history mixes run identities")]
    MixedRun,
    #[error("workflow run event time moved backwards")]
    NonMonotonicTime,
    #[error("workflow run command identity was reused")]
    DuplicateCommand,
    #[error("workflow run transition is invalid")]
    InvalidTransition,
    #[error("workflow run references an unknown step")]
    UnknownStep,
    #[error("workflow step outputs do not match the definition")]
    InvalidStepOutputs,
    #[error("workflow step inputs are not available")]
    InputsUnavailable,
    #[error("workflow definition is invalid")]
    InvalidDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunEventContextV1 {
    pub command_id: WorkCommandId,
    pub input_digest: ManifestDigest,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepOutputV1 {
    pub output_name: WorkflowOutputName,
    pub artifact: WorkArtifactRefV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatusV1 {
    Running,
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowRunStatusV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepStatusV1 {
    Blocked,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum WorkflowRunCommandV1 {
    StartStep {
        step_id: WorkflowStepId,
    },
    CompleteStep {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutputV1>,
    },
    FailStep {
        step_id: WorkflowStepId,
    },
    Pause,
    Resume,
    RequestCancellation,
    ReconcileCancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowRunEventKindV1 {
    Admitted {
        definition: WorkflowDefinitionV1,
        pinned_topology_digest: ManifestDigest,
    },
    StepStarted {
        step_id: WorkflowStepId,
    },
    StepCompleted {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutputV1>,
    },
    StepFailed {
        step_id: WorkflowStepId,
    },
    Paused,
    Resumed,
    CancellationRequested,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunEventV1 {
    run_id: RunId,
    sequence: u64,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
    event: WorkflowRunEventKindV1,
}

impl WorkflowRunEventV1 {
    pub fn admitted(
        run_id: RunId,
        definition: WorkflowDefinitionV1,
        pinned_topology_digest: ManifestDigest,
        context: WorkflowRunEventContextV1,
    ) -> Result<Self, WorkflowRunStateError> {
        definition
            .validate()
            .map_err(|_| WorkflowRunStateError::InvalidDefinition)?;
        Ok(Self {
            run_id,
            sequence: 1,
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event: WorkflowRunEventKindV1::Admitted {
                definition,
                pinned_topology_digest,
            },
        })
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn command_id(&self) -> &WorkCommandId {
        &self.command_id
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub const fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn event(&self) -> &WorkflowRunEventKindV1 {
        &self.event
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepRunProjectionV1 {
    status: WorkflowStepStatusV1,
    outputs: BTreeMap<WorkflowOutputName, WorkArtifactRefV1>,
}

impl WorkflowStepRunProjectionV1 {
    pub const fn status(&self) -> WorkflowStepStatusV1 {
        self.status
    }

    pub fn outputs(&self) -> &BTreeMap<WorkflowOutputName, WorkArtifactRefV1> {
        &self.outputs
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunProjectionV1 {
    run_id: RunId,
    definition: WorkflowDefinitionV1,
    pinned_topology_digest: ManifestDigest,
    status: WorkflowRunStatusV1,
    sequence: u64,
    steps: BTreeMap<WorkflowStepId, WorkflowStepRunProjectionV1>,
    history: Vec<WorkflowRunEventV1>,
}

impl WorkflowRunProjectionV1 {
    pub fn rebuild(history: &[WorkflowRunEventV1]) -> Result<Self, WorkflowRunStateError> {
        let first = history.first().ok_or(WorkflowRunStateError::EmptyHistory)?;
        let WorkflowRunEventKindV1::Admitted {
            definition,
            pinned_topology_digest,
        } = first.event()
        else {
            return Err(WorkflowRunStateError::InvalidTransition);
        };
        if first.sequence() != 1 {
            return Err(WorkflowRunStateError::NonContiguousSequence);
        }
        definition
            .validate()
            .map_err(|_| WorkflowRunStateError::InvalidDefinition)?;
        let mut steps = BTreeMap::new();
        for step in definition.steps() {
            steps.insert(
                step.step_id.clone(),
                WorkflowStepRunProjectionV1 {
                    status: if step.predecessors.is_empty() {
                        WorkflowStepStatusV1::Ready
                    } else {
                        WorkflowStepStatusV1::Blocked
                    },
                    outputs: BTreeMap::new(),
                },
            );
        }
        let mut projection = Self {
            run_id: first.run_id().clone(),
            definition: definition.clone(),
            pinned_topology_digest: pinned_topology_digest.clone(),
            status: WorkflowRunStatusV1::Running,
            sequence: 1,
            steps,
            history: vec![first.clone()],
        };
        for event in &history[1..] {
            projection = projection.apply(event)?;
        }
        Ok(projection)
    }

    pub fn next_event(
        &self,
        command: WorkflowRunCommandV1,
        context: WorkflowRunEventContextV1,
    ) -> Result<WorkflowRunEventV1, WorkflowRunStateError> {
        let event = match command {
            WorkflowRunCommandV1::StartStep { step_id } => {
                self.require_step_status(&step_id, WorkflowStepStatusV1::Ready)?;
                WorkflowRunEventKindV1::StepStarted { step_id }
            }
            WorkflowRunCommandV1::CompleteStep { step_id, outputs } => {
                self.require_step_status(&step_id, WorkflowStepStatusV1::Running)?;
                self.validate_outputs(&step_id, &outputs)?;
                WorkflowRunEventKindV1::StepCompleted { step_id, outputs }
            }
            WorkflowRunCommandV1::FailStep { step_id } => {
                self.require_step_status(&step_id, WorkflowStepStatusV1::Running)?;
                WorkflowRunEventKindV1::StepFailed { step_id }
            }
            WorkflowRunCommandV1::Pause => {
                if self.status != WorkflowRunStatusV1::Running {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKindV1::Paused
            }
            WorkflowRunCommandV1::Resume => {
                if self.status != WorkflowRunStatusV1::Paused {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKindV1::Resumed
            }
            WorkflowRunCommandV1::RequestCancellation => {
                if self.status.is_terminal() {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKindV1::CancellationRequested
            }
            WorkflowRunCommandV1::ReconcileCancelled => {
                if self.status != WorkflowRunStatusV1::Cancelling {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKindV1::Cancelled
            }
        };
        let next = Self::event(
            self.run_id.clone(),
            self.sequence
                .checked_add(1)
                .ok_or(WorkflowRunStateError::NonContiguousSequence)?,
            context,
            event,
        );
        self.apply(&next)?;
        Ok(next)
    }

    fn event(
        run_id: RunId,
        sequence: u64,
        context: WorkflowRunEventContextV1,
        event: WorkflowRunEventKindV1,
    ) -> WorkflowRunEventV1 {
        WorkflowRunEventV1 {
            run_id,
            sequence,
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event,
        }
    }

    pub fn apply(&self, event: &WorkflowRunEventV1) -> Result<Self, WorkflowRunStateError> {
        self.validate_envelope(event)?;
        let mut next = self.clone();
        match event.event() {
            WorkflowRunEventKindV1::Admitted { .. } => {
                return Err(WorkflowRunStateError::InvalidTransition);
            }
            WorkflowRunEventKindV1::StepStarted { step_id } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Ready)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Running;
            }
            WorkflowRunEventKindV1::StepCompleted { step_id, outputs } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Running)?;
                next.validate_outputs(step_id, outputs)?;
                let step = next.step_mut(step_id)?;
                step.status = WorkflowStepStatusV1::Succeeded;
                step.outputs = outputs
                    .iter()
                    .map(|output| (output.output_name.clone(), output.artifact.clone()))
                    .collect();
                next.release_dependents();
                if next
                    .steps
                    .values()
                    .all(|step| step.status == WorkflowStepStatusV1::Succeeded)
                {
                    next.status = WorkflowRunStatusV1::Completed;
                }
            }
            WorkflowRunEventKindV1::StepFailed { step_id } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Running)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Failed;
                next.status = WorkflowRunStatusV1::Failed;
            }
            WorkflowRunEventKindV1::Paused => {
                next.require_running()?;
                next.status = WorkflowRunStatusV1::Paused;
            }
            WorkflowRunEventKindV1::Resumed => {
                if next.status != WorkflowRunStatusV1::Paused {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                next.status = WorkflowRunStatusV1::Running;
            }
            WorkflowRunEventKindV1::CancellationRequested => {
                if next.status.is_terminal() {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                next.status = WorkflowRunStatusV1::Cancelling;
            }
            WorkflowRunEventKindV1::Cancelled => {
                if next.status != WorkflowRunStatusV1::Cancelling {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                for step in next.steps.values_mut() {
                    if !matches!(
                        step.status,
                        WorkflowStepStatusV1::Succeeded | WorkflowStepStatusV1::Failed
                    ) {
                        step.status = WorkflowStepStatusV1::Cancelled;
                    }
                }
                next.status = WorkflowRunStatusV1::Cancelled;
            }
        }
        next.sequence = event.sequence();
        next.history.push(event.clone());
        Ok(next)
    }

    fn validate_envelope(&self, event: &WorkflowRunEventV1) -> Result<(), WorkflowRunStateError> {
        if event.run_id() != &self.run_id {
            return Err(WorkflowRunStateError::MixedRun);
        }
        if event.sequence() != self.sequence.saturating_add(1) {
            return Err(WorkflowRunStateError::NonContiguousSequence);
        }
        if event.occurred_at() < self.last_occurred_at()? {
            return Err(WorkflowRunStateError::NonMonotonicTime);
        }
        if self
            .history
            .iter()
            .any(|admitted| admitted.command_id() == event.command_id())
        {
            return Err(WorkflowRunStateError::DuplicateCommand);
        }
        Ok(())
    }

    fn validate_outputs(
        &self,
        step_id: &WorkflowStepId,
        outputs: &[WorkflowStepOutputV1],
    ) -> Result<(), WorkflowRunStateError> {
        let definition = self
            .definition
            .steps()
            .iter()
            .find(|step| &step.step_id == step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)?;
        let declared = definition.outputs.iter().collect::<BTreeSet<_>>();
        let actual = outputs
            .iter()
            .map(|output| &output.output_name)
            .collect::<BTreeSet<_>>();
        if outputs.len() != actual.len() || actual != declared {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(())
    }

    fn release_dependents(&mut self) {
        for definition_step in self.definition.steps() {
            if self
                .steps
                .get(&definition_step.step_id)
                .map(|step| step.status)
                != Some(WorkflowStepStatusV1::Blocked)
            {
                continue;
            }
            if definition_step.predecessors.iter().all(|predecessor| {
                self.steps.get(predecessor).map(|step| step.status)
                    == Some(WorkflowStepStatusV1::Succeeded)
            }) && let Some(step) = self.steps.get_mut(&definition_step.step_id)
            {
                step.status = WorkflowStepStatusV1::Ready;
            }
        }
    }

    fn require_running(&self) -> Result<(), WorkflowRunStateError> {
        if self.status != WorkflowRunStatusV1::Running {
            return Err(WorkflowRunStateError::InvalidTransition);
        }
        Ok(())
    }

    fn require_step_status(
        &self,
        step_id: &WorkflowStepId,
        status: WorkflowStepStatusV1,
    ) -> Result<(), WorkflowRunStateError> {
        if self
            .steps
            .get(step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)?
            .status
            != status
        {
            return Err(WorkflowRunStateError::InvalidTransition);
        }
        Ok(())
    }

    fn step_mut(
        &mut self,
        step_id: &WorkflowStepId,
    ) -> Result<&mut WorkflowStepRunProjectionV1, WorkflowRunStateError> {
        self.steps
            .get_mut(step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)
    }

    fn last_occurred_at(&self) -> Result<UtcMicros, WorkflowRunStateError> {
        self.history
            .last()
            .map(WorkflowRunEventV1::occurred_at)
            .ok_or(WorkflowRunStateError::EmptyHistory)
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn definition(&self) -> &WorkflowDefinitionV1 {
        &self.definition
    }

    pub fn pinned_topology_digest(&self) -> &ManifestDigest {
        &self.pinned_topology_digest
    }

    pub const fn status(&self) -> WorkflowRunStatusV1 {
        self.status
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn history(&self) -> &[WorkflowRunEventV1] {
        &self.history
    }

    pub fn ready_steps(&self) -> Vec<WorkflowStepId> {
        self.steps
            .iter()
            .filter_map(|(step_id, step)| {
                (step.status == WorkflowStepStatusV1::Ready).then_some(step_id.clone())
            })
            .collect()
    }

    pub fn resolved_inputs(
        &self,
        step_id: &WorkflowStepId,
    ) -> Result<Vec<WorkArtifactRefV1>, WorkflowRunStateError> {
        let definition = self
            .definition
            .steps()
            .iter()
            .find(|step| &step.step_id == step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)?;
        definition
            .inputs
            .iter()
            .map(|reference| self.resolve_input(reference))
            .collect()
    }

    fn resolve_input(
        &self,
        reference: &WorkflowOutputReferenceV1,
    ) -> Result<WorkArtifactRefV1, WorkflowRunStateError> {
        self.steps
            .get(&reference.producer_step_id)
            .and_then(|step| step.outputs.get(&reference.output_name))
            .cloned()
            .ok_or(WorkflowRunStateError::InputsUnavailable)
    }
}
