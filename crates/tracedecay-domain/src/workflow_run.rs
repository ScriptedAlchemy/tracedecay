//! Event-journaled Workflow run state over immutable definitions.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ManifestDigest, RunId, UtcMicros, WorkArtifactRefV1, WorkAttemptIdentityV1, WorkCommandId,
    WorkflowDefinition, WorkflowOutputName, WorkflowOutputReference, WorkflowPlacementReceipt,
    WorkflowStepEffectOutcome, WorkflowStepEffectReceipt, WorkflowStepId,
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
    #[error("workflow placement receipt is invalid or stale")]
    InvalidPlacementReceipt,
    #[error("workflow step effect receipt is invalid or stale")]
    InvalidEffectReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunEventContext {
    pub command_id: WorkCommandId,
    pub input_digest: ManifestDigest,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowOutputArtifact {
    attempt_identity: WorkAttemptIdentityV1,
    artifact: WorkArtifactRefV1,
}

impl WorkflowOutputArtifact {
    pub const fn new(attempt_identity: WorkAttemptIdentityV1, artifact: WorkArtifactRefV1) -> Self {
        Self {
            attempt_identity,
            artifact,
        }
    }

    pub fn attempt_identity(&self) -> &WorkAttemptIdentityV1 {
        &self.attempt_identity
    }

    pub fn artifact(&self) -> &WorkArtifactRefV1 {
        &self.artifact
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepOutput {
    output_name: WorkflowOutputName,
    artifacts: Vec<WorkflowOutputArtifact>,
}

impl WorkflowStepOutput {
    pub fn new(
        output_name: WorkflowOutputName,
        mut artifacts: Vec<WorkflowOutputArtifact>,
    ) -> Result<Self, WorkflowRunStateError> {
        artifacts.sort_by(|left, right| left.attempt_identity.cmp(&right.attempt_identity));
        let attempt_count = artifacts
            .iter()
            .map(|artifact| artifact.attempt_identity())
            .collect::<BTreeSet<_>>()
            .len();
        let artifact_count = artifacts
            .iter()
            .map(|artifact| artifact.artifact().artifact_id())
            .collect::<BTreeSet<_>>()
            .len();
        if artifacts.is_empty()
            || attempt_count != artifacts.len()
            || artifact_count != artifacts.len()
        {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(Self {
            output_name,
            artifacts,
        })
    }

    pub fn output_name(&self) -> &WorkflowOutputName {
        &self.output_name
    }

    pub fn artifacts(&self) -> &[WorkflowOutputArtifact] {
        &self.artifacts
    }

    fn validate(&self) -> Result<(), WorkflowRunStateError> {
        if Self::new(self.output_name.clone(), self.artifacts.clone())? != *self {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkflowStepOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            output_name: WorkflowOutputName,
            artifacts: Vec<WorkflowOutputArtifact>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.output_name, wire.artifacts).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepInput {
    reference: WorkflowOutputReference,
    artifacts: Vec<WorkflowOutputArtifact>,
}

impl WorkflowStepInput {
    fn from_output(
        reference: WorkflowOutputReference,
        output: &WorkflowStepOutput,
    ) -> Result<Self, WorkflowRunStateError> {
        output.validate()?;
        Ok(Self {
            reference,
            artifacts: output.artifacts.clone(),
        })
    }

    pub fn reference(&self) -> &WorkflowOutputReference {
        &self.reference
    }

    pub fn artifacts(&self) -> &[WorkflowOutputArtifact] {
        &self.artifacts
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl WorkflowRunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepStatus {
    Blocked,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum WorkflowRunCommand {
    StartStep {
        step_id: WorkflowStepId,
        placement: WorkflowPlacementReceipt,
    },
    CompleteStep {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutput>,
        effect_receipt: WorkflowStepEffectReceipt,
    },
    FailStep {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutput>,
        effect_receipt: WorkflowStepEffectReceipt,
    },
    Pause,
    Resume,
    RequestCancellation,
    ReconcileCancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowRunEventKind {
    Admitted {
        definition: WorkflowDefinition,
        pinned_topology_digest: ManifestDigest,
        pinned_provider_registry_digest: ManifestDigest,
    },
    StepStarted {
        step_id: WorkflowStepId,
        placement: WorkflowPlacementReceipt,
    },
    StepCompleted {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutput>,
        effect_receipt: WorkflowStepEffectReceipt,
    },
    StepFailed {
        step_id: WorkflowStepId,
        outputs: Vec<WorkflowStepOutput>,
        effect_receipt: WorkflowStepEffectReceipt,
    },
    Paused,
    Resumed,
    CancellationRequested,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunEvent {
    run_id: RunId,
    sequence: u64,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
    event: WorkflowRunEventKind,
}

impl WorkflowRunEvent {
    pub fn admitted(
        run_id: RunId,
        definition: WorkflowDefinition,
        pinned_topology_digest: ManifestDigest,
        pinned_provider_registry_digest: ManifestDigest,
        context: WorkflowRunEventContext,
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
            event: WorkflowRunEventKind::Admitted {
                definition,
                pinned_topology_digest,
                pinned_provider_registry_digest,
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

    pub fn event(&self) -> &WorkflowRunEventKind {
        &self.event
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepRunProjection {
    status: WorkflowStepStatus,
    outputs: BTreeMap<WorkflowOutputName, WorkflowStepOutput>,
    placement_receipt: Option<WorkflowPlacementReceipt>,
    effect_receipt: Option<WorkflowStepEffectReceipt>,
}

impl WorkflowStepRunProjection {
    pub const fn status(&self) -> WorkflowStepStatus {
        self.status
    }

    pub fn outputs(&self) -> &BTreeMap<WorkflowOutputName, WorkflowStepOutput> {
        &self.outputs
    }

    pub fn placement_receipt(&self) -> Option<&WorkflowPlacementReceipt> {
        self.placement_receipt.as_ref()
    }

    pub fn effect_receipt(&self) -> Option<&WorkflowStepEffectReceipt> {
        self.effect_receipt.as_ref()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunProjection {
    run_id: RunId,
    definition: WorkflowDefinition,
    pinned_topology_digest: ManifestDigest,
    pinned_provider_registry_digest: ManifestDigest,
    status: WorkflowRunStatus,
    sequence: u64,
    steps: BTreeMap<WorkflowStepId, WorkflowStepRunProjection>,
    history: Vec<WorkflowRunEvent>,
}

impl WorkflowRunProjection {
    pub fn rebuild(history: &[WorkflowRunEvent]) -> Result<Self, WorkflowRunStateError> {
        let first = history.first().ok_or(WorkflowRunStateError::EmptyHistory)?;
        let WorkflowRunEventKind::Admitted {
            definition,
            pinned_topology_digest,
            pinned_provider_registry_digest,
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
                WorkflowStepRunProjection {
                    status: if step.predecessors.is_empty() {
                        WorkflowStepStatus::Ready
                    } else {
                        WorkflowStepStatus::Blocked
                    },
                    outputs: BTreeMap::new(),
                    placement_receipt: None,
                    effect_receipt: None,
                },
            );
        }
        let mut projection = Self {
            run_id: first.run_id().clone(),
            definition: definition.clone(),
            pinned_topology_digest: pinned_topology_digest.clone(),
            pinned_provider_registry_digest: pinned_provider_registry_digest.clone(),
            status: WorkflowRunStatus::Running,
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
        command: WorkflowRunCommand,
        context: WorkflowRunEventContext,
    ) -> Result<WorkflowRunEvent, WorkflowRunStateError> {
        let event = match command {
            WorkflowRunCommand::StartStep { step_id, placement } => {
                self.require_step_status(&step_id, WorkflowStepStatus::Ready)?;
                self.validate_placement(&step_id, &placement)?;
                WorkflowRunEventKind::StepStarted { step_id, placement }
            }
            WorkflowRunCommand::CompleteStep {
                step_id,
                outputs,
                effect_receipt,
            } => {
                self.require_step_status(&step_id, WorkflowStepStatus::Running)?;
                self.validate_outputs(&step_id, &outputs)?;
                self.validate_effect_receipt(&step_id, &effect_receipt, Some(&outputs))?;
                if effect_receipt.outcome() != WorkflowStepEffectOutcome::Completed {
                    return Err(WorkflowRunStateError::InvalidEffectReceipt);
                }
                WorkflowRunEventKind::StepCompleted {
                    step_id,
                    outputs,
                    effect_receipt,
                }
            }
            WorkflowRunCommand::FailStep {
                step_id,
                outputs,
                effect_receipt,
            } => {
                self.require_step_status(&step_id, WorkflowStepStatus::Running)?;
                self.validate_failure_outputs(&step_id, &outputs)?;
                self.validate_effect_receipt(&step_id, &effect_receipt, Some(&outputs))?;
                if effect_receipt.outcome() != WorkflowStepEffectOutcome::Failed {
                    return Err(WorkflowRunStateError::InvalidEffectReceipt);
                }
                WorkflowRunEventKind::StepFailed {
                    step_id,
                    outputs,
                    effect_receipt,
                }
            }
            WorkflowRunCommand::Pause => {
                if self.status != WorkflowRunStatus::Running {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKind::Paused
            }
            WorkflowRunCommand::Resume => {
                if self.status != WorkflowRunStatus::Paused {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKind::Resumed
            }
            WorkflowRunCommand::RequestCancellation => {
                if self.status.is_terminal() {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKind::CancellationRequested
            }
            WorkflowRunCommand::ReconcileCancelled => {
                if self.status != WorkflowRunStatus::Cancelling {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                WorkflowRunEventKind::Cancelled
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
        context: WorkflowRunEventContext,
        event: WorkflowRunEventKind,
    ) -> WorkflowRunEvent {
        WorkflowRunEvent {
            run_id,
            sequence,
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event,
        }
    }

    pub fn apply(&self, event: &WorkflowRunEvent) -> Result<Self, WorkflowRunStateError> {
        self.validate_envelope(event)?;
        let mut next = self.clone();
        match event.event() {
            WorkflowRunEventKind::Admitted { .. } => {
                return Err(WorkflowRunStateError::InvalidTransition);
            }
            WorkflowRunEventKind::StepStarted { step_id, placement } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatus::Ready)?;
                next.validate_placement(step_id, placement)?;
                let step = next.step_mut(step_id)?;
                step.status = WorkflowStepStatus::Running;
                step.placement_receipt = Some(placement.clone());
            }
            WorkflowRunEventKind::StepCompleted {
                step_id,
                outputs,
                effect_receipt,
            } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatus::Running)?;
                next.validate_outputs(step_id, outputs)?;
                next.validate_effect_receipt(step_id, effect_receipt, Some(outputs))?;
                if effect_receipt.outcome() != WorkflowStepEffectOutcome::Completed {
                    return Err(WorkflowRunStateError::InvalidEffectReceipt);
                }
                let step = next.step_mut(step_id)?;
                step.status = WorkflowStepStatus::Succeeded;
                step.outputs = outputs
                    .iter()
                    .map(|output| (output.output_name.clone(), output.clone()))
                    .collect();
                step.effect_receipt = Some(effect_receipt.clone());
                next.release_dependents();
                if next
                    .steps
                    .values()
                    .all(|step| step.status == WorkflowStepStatus::Succeeded)
                {
                    next.status = WorkflowRunStatus::Completed;
                }
            }
            WorkflowRunEventKind::StepFailed {
                step_id,
                outputs,
                effect_receipt,
            } => {
                next.require_running()?;
                next.require_step_status(step_id, WorkflowStepStatus::Running)?;
                next.validate_failure_outputs(step_id, outputs)?;
                next.validate_effect_receipt(step_id, effect_receipt, Some(outputs))?;
                if effect_receipt.outcome() != WorkflowStepEffectOutcome::Failed {
                    return Err(WorkflowRunStateError::InvalidEffectReceipt);
                }
                let step = next.step_mut(step_id)?;
                step.status = WorkflowStepStatus::Failed;
                step.outputs = outputs
                    .iter()
                    .map(|output| (output.output_name.clone(), output.clone()))
                    .collect();
                step.effect_receipt = Some(effect_receipt.clone());
                next.status = WorkflowRunStatus::Failed;
            }
            WorkflowRunEventKind::Paused => {
                next.require_running()?;
                next.status = WorkflowRunStatus::Paused;
            }
            WorkflowRunEventKind::Resumed => {
                if next.status != WorkflowRunStatus::Paused {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                next.status = WorkflowRunStatus::Running;
            }
            WorkflowRunEventKind::CancellationRequested => {
                if next.status.is_terminal() {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                next.status = WorkflowRunStatus::Cancelling;
            }
            WorkflowRunEventKind::Cancelled => {
                if next.status != WorkflowRunStatus::Cancelling {
                    return Err(WorkflowRunStateError::InvalidTransition);
                }
                for step in next.steps.values_mut() {
                    if !matches!(
                        step.status,
                        WorkflowStepStatus::Succeeded | WorkflowStepStatus::Failed
                    ) {
                        step.status = WorkflowStepStatus::Cancelled;
                    }
                }
                next.status = WorkflowRunStatus::Cancelled;
            }
        }
        next.sequence = event.sequence();
        next.history.push(event.clone());
        Ok(next)
    }

    fn validate_envelope(&self, event: &WorkflowRunEvent) -> Result<(), WorkflowRunStateError> {
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
        outputs: &[WorkflowStepOutput],
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
            .map(WorkflowStepOutput::output_name)
            .collect::<BTreeSet<_>>();
        let first_attempts = outputs.first().map(|output| {
            output
                .artifacts()
                .iter()
                .map(|artifact| artifact.attempt_identity())
                .collect::<BTreeSet<_>>()
        });
        let artifact_sets_match = first_attempts.as_ref().is_none_or(|expected| {
            outputs.iter().all(|output| {
                output
                    .artifacts()
                    .iter()
                    .map(|artifact| artifact.attempt_identity())
                    .collect::<BTreeSet<_>>()
                    == *expected
            })
        });
        let artifact_count = first_attempts.as_ref().map_or(0, BTreeSet::len);
        let width_is_valid = match definition.fan_out {
            Some(fan_out) => artifact_count <= fan_out.max_width as usize,
            None => artifact_count == 1,
        };
        if outputs.len() != actual.len()
            || actual != declared
            || outputs.iter().any(|output| output.validate().is_err())
            || !artifact_sets_match
            || !width_is_valid
        {
            return Err(WorkflowRunStateError::InvalidStepOutputs);
        }
        Ok(())
    }

    fn validate_failure_outputs(
        &self,
        step_id: &WorkflowStepId,
        outputs: &[WorkflowStepOutput],
    ) -> Result<(), WorkflowRunStateError> {
        if outputs.is_empty() {
            return Ok(());
        }
        self.validate_outputs(step_id, outputs)
    }

    fn validate_placement(
        &self,
        step_id: &WorkflowStepId,
        placement: &WorkflowPlacementReceipt,
    ) -> Result<(), WorkflowRunStateError> {
        placement
            .validate()
            .map_err(|_| WorkflowRunStateError::InvalidPlacementReceipt)?;
        if placement.run_id() != &self.run_id
            || placement.step_id() != step_id
            || placement.configuration_digest() != self.definition.pinned_configuration_digest()
            || placement.topology_digest() != &self.pinned_topology_digest
            || placement.provider_registry_digest() != &self.pinned_provider_registry_digest
        {
            return Err(WorkflowRunStateError::InvalidPlacementReceipt);
        }
        Ok(())
    }

    fn validate_effect_receipt(
        &self,
        step_id: &WorkflowStepId,
        effect_receipt: &WorkflowStepEffectReceipt,
        outputs: Option<&[WorkflowStepOutput]>,
    ) -> Result<(), WorkflowRunStateError> {
        let step = self
            .steps
            .get(step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)?;
        let placement = step
            .placement_receipt
            .as_ref()
            .ok_or(WorkflowRunStateError::InvalidEffectReceipt)?;
        effect_receipt
            .validate()
            .map_err(|_| WorkflowRunStateError::InvalidEffectReceipt)?;
        if let Some(outputs) = outputs {
            effect_receipt
                .validate_outputs(outputs)
                .map_err(|_| WorkflowRunStateError::InvalidEffectReceipt)?;
        }
        if effect_receipt.run_id() != &self.run_id
            || effect_receipt.step_id() != step_id
            || effect_receipt.placement_digest() != placement.placement_digest()
        {
            return Err(WorkflowRunStateError::InvalidEffectReceipt);
        }
        Ok(())
    }

    fn release_dependents(&mut self) {
        for definition_step in self.definition.steps() {
            if self
                .steps
                .get(&definition_step.step_id)
                .map(|step| step.status)
                != Some(WorkflowStepStatus::Blocked)
            {
                continue;
            }
            if definition_step.predecessors.iter().all(|predecessor| {
                self.steps.get(predecessor).map(|step| step.status)
                    == Some(WorkflowStepStatus::Succeeded)
            }) && let Some(step) = self.steps.get_mut(&definition_step.step_id)
            {
                step.status = WorkflowStepStatus::Ready;
            }
        }
    }

    fn require_running(&self) -> Result<(), WorkflowRunStateError> {
        if self.status != WorkflowRunStatus::Running {
            return Err(WorkflowRunStateError::InvalidTransition);
        }
        Ok(())
    }

    fn require_step_status(
        &self,
        step_id: &WorkflowStepId,
        status: WorkflowStepStatus,
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
    ) -> Result<&mut WorkflowStepRunProjection, WorkflowRunStateError> {
        self.steps
            .get_mut(step_id)
            .ok_or(WorkflowRunStateError::UnknownStep)
    }

    fn last_occurred_at(&self) -> Result<UtcMicros, WorkflowRunStateError> {
        self.history
            .last()
            .map(WorkflowRunEvent::occurred_at)
            .ok_or(WorkflowRunStateError::EmptyHistory)
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn definition(&self) -> &WorkflowDefinition {
        &self.definition
    }

    pub fn pinned_topology_digest(&self) -> &ManifestDigest {
        &self.pinned_topology_digest
    }

    pub fn pinned_provider_registry_digest(&self) -> &ManifestDigest {
        &self.pinned_provider_registry_digest
    }

    pub const fn status(&self) -> WorkflowRunStatus {
        self.status
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn history(&self) -> &[WorkflowRunEvent] {
        &self.history
    }

    pub fn step(&self, step_id: &WorkflowStepId) -> Option<&WorkflowStepRunProjection> {
        self.steps.get(step_id)
    }

    pub fn ready_steps(&self) -> Vec<WorkflowStepId> {
        self.steps
            .iter()
            .filter_map(|(step_id, step)| {
                (step.status == WorkflowStepStatus::Ready).then_some(step_id.clone())
            })
            .collect()
    }

    pub fn resolved_inputs(
        &self,
        step_id: &WorkflowStepId,
    ) -> Result<Vec<WorkflowStepInput>, WorkflowRunStateError> {
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
        reference: &WorkflowOutputReference,
    ) -> Result<WorkflowStepInput, WorkflowRunStateError> {
        self.steps
            .get(&reference.producer_step_id)
            .and_then(|step| step.outputs.get(&reference.output_name))
            .ok_or(WorkflowRunStateError::InputsUnavailable)
            .and_then(|output| WorkflowStepInput::from_output(reference.clone(), output))
    }
}
