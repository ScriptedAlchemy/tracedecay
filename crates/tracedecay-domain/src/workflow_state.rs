//! Event-sourced workflow-definition lifecycle and workflow-run projections.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ManifestDigest, RunId, UtcMicros, WorkCommandId, WorkflowDefinitionId, WorkflowDefinitionV1,
    WorkflowStepId,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStateError {
    #[error("workflow aggregate version must be non-zero")]
    InvalidAggregateVersion,
    #[error("workflow aggregate version overflowed")]
    AggregateVersionOverflow,
    #[error("workflow history must not be empty")]
    EmptyHistory,
    #[error("workflow history has a non-contiguous aggregate version")]
    NonContiguousVersion,
    #[error("workflow history mixes aggregate identities")]
    MixedAggregate,
    #[error("workflow event times must be monotonic")]
    NonMonotonicTime,
    #[error("workflow command identity is duplicated")]
    DuplicateCommand,
    #[error("workflow transition is invalid for the current state")]
    InvalidTransition,
    #[error("workflow definition must be active to start a run")]
    DefinitionNotActive,
    #[error("workflow definition approval gate references an unknown step")]
    UnknownApprovalStep,
    #[error("workflow run references an unknown step")]
    UnknownStep,
    #[error("workflow step approval was not requested")]
    ApprovalNotRequested,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkflowAggregateVersionV1(u64);

impl WorkflowAggregateVersionV1 {
    pub fn new(value: u64) -> Result<Self, WorkflowStateError> {
        if value == 0 {
            return Err(WorkflowStateError::InvalidAggregateVersion);
        }
        Ok(Self(value))
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, WorkflowStateError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WorkflowStateError::AggregateVersionOverflow)
    }
}

impl<'de> Deserialize<'de> for WorkflowAggregateVersionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCommandContextV1 {
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
}

impl WorkflowCommandContextV1 {
    pub fn new(
        command_id: WorkCommandId,
        input_digest: ManifestDigest,
        occurred_at: UtcMicros,
    ) -> Self {
        Self {
            command_id,
            input_digest,
            occurred_at,
        }
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
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDefinitionStatusV1 {
    Candidate,
    Validated,
    Active,
    Retired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDefinitionCommandV1 {
    Validate,
    Activate,
    Retire,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowDefinitionEventKindV1 {
    Registered {
        definition: WorkflowDefinitionV1,
        approval_steps: BTreeSet<WorkflowStepId>,
    },
    Validated,
    Activated,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionEventV1 {
    definition_id: WorkflowDefinitionId,
    definition_version: u64,
    aggregate_version: WorkflowAggregateVersionV1,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
    event: WorkflowDefinitionEventKindV1,
}

impl WorkflowDefinitionEventV1 {
    pub fn definition_id(&self) -> &WorkflowDefinitionId {
        &self.definition_id
    }

    pub const fn definition_version(&self) -> u64 {
        self.definition_version
    }

    pub const fn aggregate_version(&self) -> WorkflowAggregateVersionV1 {
        self.aggregate_version
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

    pub fn event(&self) -> &WorkflowDefinitionEventKindV1 {
        &self.event
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionProjectionV1 {
    definition: WorkflowDefinitionV1,
    approval_steps: BTreeSet<WorkflowStepId>,
    status: WorkflowDefinitionStatusV1,
    aggregate_version: WorkflowAggregateVersionV1,
    history: Vec<WorkflowDefinitionEventV1>,
}

impl WorkflowDefinitionProjectionV1 {
    pub fn register(
        definition: WorkflowDefinitionV1,
        approval_steps: BTreeSet<WorkflowStepId>,
        context: WorkflowCommandContextV1,
    ) -> Result<Self, WorkflowStateError> {
        let step_ids = definition
            .steps()
            .iter()
            .map(|step| &step.step_id)
            .collect::<BTreeSet<_>>();
        if approval_steps.iter().any(|step| !step_ids.contains(step)) {
            return Err(WorkflowStateError::UnknownApprovalStep);
        }
        let event = WorkflowDefinitionEventV1 {
            definition_id: definition.definition_id().clone(),
            definition_version: definition.definition_version(),
            aggregate_version: WorkflowAggregateVersionV1::initial(),
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event: WorkflowDefinitionEventKindV1::Registered {
                definition,
                approval_steps,
            },
        };
        Self::rebuild(&[event])
    }

    pub fn rebuild(history: &[WorkflowDefinitionEventV1]) -> Result<Self, WorkflowStateError> {
        let first = history.first().ok_or(WorkflowStateError::EmptyHistory)?;
        let WorkflowDefinitionEventKindV1::Registered {
            definition,
            approval_steps,
        } = first.event()
        else {
            return Err(WorkflowStateError::InvalidTransition);
        };
        if first.aggregate_version() != WorkflowAggregateVersionV1::initial()
            || first.definition_id() != definition.definition_id()
            || first.definition_version() != definition.definition_version()
        {
            return Err(WorkflowStateError::MixedAggregate);
        }
        let mut projection = Self {
            definition: definition.clone(),
            approval_steps: approval_steps.clone(),
            status: WorkflowDefinitionStatusV1::Candidate,
            aggregate_version: first.aggregate_version(),
            history: vec![first.clone()],
        };
        for event in &history[1..] {
            projection = projection.apply(event)?;
        }
        Ok(projection)
    }

    pub fn handle(
        &self,
        command: WorkflowDefinitionCommandV1,
        context: WorkflowCommandContextV1,
    ) -> Result<Self, WorkflowStateError> {
        let event = match (self.status, command) {
            (WorkflowDefinitionStatusV1::Candidate, WorkflowDefinitionCommandV1::Validate) => {
                WorkflowDefinitionEventKindV1::Validated
            }
            (WorkflowDefinitionStatusV1::Validated, WorkflowDefinitionCommandV1::Activate) => {
                WorkflowDefinitionEventKindV1::Activated
            }
            (WorkflowDefinitionStatusV1::Active, WorkflowDefinitionCommandV1::Retire) => {
                WorkflowDefinitionEventKindV1::Retired
            }
            _ => return Err(WorkflowStateError::InvalidTransition),
        };
        self.apply(&WorkflowDefinitionEventV1 {
            definition_id: self.definition.definition_id().clone(),
            definition_version: self.definition.definition_version(),
            aggregate_version: self.aggregate_version.next()?,
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event,
        })
    }

    pub fn apply(&self, event: &WorkflowDefinitionEventV1) -> Result<Self, WorkflowStateError> {
        self.validate_envelope(
            event.definition_id(),
            event.definition_version(),
            event.aggregate_version(),
            event.command_id(),
            event.occurred_at(),
        )?;
        let status = match (self.status, event.event()) {
            (WorkflowDefinitionStatusV1::Candidate, WorkflowDefinitionEventKindV1::Validated) => {
                WorkflowDefinitionStatusV1::Validated
            }
            (WorkflowDefinitionStatusV1::Validated, WorkflowDefinitionEventKindV1::Activated) => {
                WorkflowDefinitionStatusV1::Active
            }
            (WorkflowDefinitionStatusV1::Active, WorkflowDefinitionEventKindV1::Retired) => {
                WorkflowDefinitionStatusV1::Retired
            }
            _ => return Err(WorkflowStateError::InvalidTransition),
        };
        let mut next = self.clone();
        next.status = status;
        next.aggregate_version = event.aggregate_version();
        next.history.push(event.clone());
        Ok(next)
    }

    fn validate_envelope(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        aggregate_version: WorkflowAggregateVersionV1,
        command_id: &WorkCommandId,
        occurred_at: UtcMicros,
    ) -> Result<(), WorkflowStateError> {
        if definition_id != self.definition.definition_id()
            || definition_version != self.definition.definition_version()
        {
            return Err(WorkflowStateError::MixedAggregate);
        }
        if aggregate_version != self.aggregate_version.next()? {
            return Err(WorkflowStateError::NonContiguousVersion);
        }
        if occurred_at < self.last_occurred_at()? {
            return Err(WorkflowStateError::NonMonotonicTime);
        }
        if self
            .history
            .iter()
            .any(|event| event.command_id() == command_id)
        {
            return Err(WorkflowStateError::DuplicateCommand);
        }
        Ok(())
    }

    pub fn definition(&self) -> &WorkflowDefinitionV1 {
        &self.definition
    }

    pub fn approval_steps(&self) -> &BTreeSet<WorkflowStepId> {
        &self.approval_steps
    }

    pub const fn status(&self) -> WorkflowDefinitionStatusV1 {
        self.status
    }

    pub const fn aggregate_version(&self) -> WorkflowAggregateVersionV1 {
        self.aggregate_version
    }

    pub fn history(&self) -> &[WorkflowDefinitionEventV1] {
        &self.history
    }

    fn last_occurred_at(&self) -> Result<UtcMicros, WorkflowStateError> {
        self.history
            .last()
            .map(WorkflowDefinitionEventV1::occurred_at)
            .ok_or(WorkflowStateError::EmptyHistory)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatusV1 {
    Running,
    Paused,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

impl WorkflowRunStatusV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepStatusV1 {
    Blocked,
    AwaitingApproval,
    Ready,
    Running,
    ReconcileRequired,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepProjectionV1 {
    status: WorkflowStepStatusV1,
    approval_requested: bool,
}

impl WorkflowStepProjectionV1 {
    pub const fn status(&self) -> WorkflowStepStatusV1 {
        self.status
    }

    pub const fn approval_requested(&self) -> bool {
        self.approval_requested
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowRunCommandV1 {
    StartStep { step_id: WorkflowStepId },
    CompleteStep { step_id: WorkflowStepId },
    FailStep { step_id: WorkflowStepId },
    RequestApproval { step_id: WorkflowStepId },
    RecordApproval { step_id: WorkflowStepId },
    RecordDenial { step_id: WorkflowStepId },
    Pause,
    Resume,
    RequestCancellation,
    ReconcileStepCancelled { step_id: WorkflowStepId },
    Restart,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowRunEventKindV1 {
    Started {
        definition: WorkflowDefinitionV1,
        approval_steps: BTreeSet<WorkflowStepId>,
    },
    StepStarted {
        step_id: WorkflowStepId,
    },
    StepSucceeded {
        step_id: WorkflowStepId,
    },
    StepFailed {
        step_id: WorkflowStepId,
    },
    ApprovalRequested {
        step_id: WorkflowStepId,
    },
    ApprovalRecorded {
        step_id: WorkflowStepId,
    },
    DenialRecorded {
        step_id: WorkflowStepId,
    },
    Paused,
    Resumed,
    CancellationRequested,
    StepReconciledCancelled {
        step_id: WorkflowStepId,
    },
    Restarted {
        authority_epoch: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunEventV1 {
    run_id: RunId,
    aggregate_version: WorkflowAggregateVersionV1,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    occurred_at: UtcMicros,
    event: WorkflowRunEventKindV1,
}

impl WorkflowRunEventV1 {
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub const fn aggregate_version(&self) -> WorkflowAggregateVersionV1 {
        self.aggregate_version
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
pub struct WorkflowRunProjectionV1 {
    run_id: RunId,
    definition: WorkflowDefinitionV1,
    approval_steps: BTreeSet<WorkflowStepId>,
    steps: BTreeMap<WorkflowStepId, WorkflowStepProjectionV1>,
    status: WorkflowRunStatusV1,
    authority_epoch: u64,
    aggregate_version: WorkflowAggregateVersionV1,
    history: Vec<WorkflowRunEventV1>,
}

impl WorkflowRunProjectionV1 {
    pub fn start(
        run_id: RunId,
        definition: &WorkflowDefinitionProjectionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<Self, WorkflowStateError> {
        if definition.status() != WorkflowDefinitionStatusV1::Active {
            return Err(WorkflowStateError::DefinitionNotActive);
        }
        Self::rebuild(&[WorkflowRunEventV1 {
            run_id,
            aggregate_version: WorkflowAggregateVersionV1::initial(),
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event: WorkflowRunEventKindV1::Started {
                definition: definition.definition().clone(),
                approval_steps: definition.approval_steps().clone(),
            },
        }])
    }

    pub fn rebuild(history: &[WorkflowRunEventV1]) -> Result<Self, WorkflowStateError> {
        let first = history.first().ok_or(WorkflowStateError::EmptyHistory)?;
        let WorkflowRunEventKindV1::Started {
            definition,
            approval_steps,
        } = first.event()
        else {
            return Err(WorkflowStateError::InvalidTransition);
        };
        if first.aggregate_version() != WorkflowAggregateVersionV1::initial() {
            return Err(WorkflowStateError::NonContiguousVersion);
        }
        let mut steps = BTreeMap::new();
        for step in definition.steps() {
            let status = if step.predecessors.is_empty() {
                Self::released_status(approval_steps, &step.step_id)
            } else {
                WorkflowStepStatusV1::Blocked
            };
            steps.insert(
                step.step_id.clone(),
                WorkflowStepProjectionV1 {
                    status,
                    approval_requested: false,
                },
            );
        }
        let mut projection = Self {
            run_id: first.run_id().clone(),
            definition: definition.clone(),
            approval_steps: approval_steps.clone(),
            steps,
            status: WorkflowRunStatusV1::Running,
            authority_epoch: 1,
            aggregate_version: first.aggregate_version(),
            history: vec![first.clone()],
        };
        for event in &history[1..] {
            projection = projection.apply(event)?;
        }
        Ok(projection)
    }

    pub fn handle(
        &self,
        command: WorkflowRunCommandV1,
        context: WorkflowCommandContextV1,
    ) -> Result<Self, WorkflowStateError> {
        let event = match command {
            WorkflowRunCommandV1::StartStep { step_id } => {
                WorkflowRunEventKindV1::StepStarted { step_id }
            }
            WorkflowRunCommandV1::CompleteStep { step_id } => {
                WorkflowRunEventKindV1::StepSucceeded { step_id }
            }
            WorkflowRunCommandV1::FailStep { step_id } => {
                WorkflowRunEventKindV1::StepFailed { step_id }
            }
            WorkflowRunCommandV1::RequestApproval { step_id } => {
                WorkflowRunEventKindV1::ApprovalRequested { step_id }
            }
            WorkflowRunCommandV1::RecordApproval { step_id } => {
                if !self
                    .steps
                    .get(&step_id)
                    .ok_or(WorkflowStateError::UnknownStep)?
                    .approval_requested
                {
                    return Err(WorkflowStateError::ApprovalNotRequested);
                }
                WorkflowRunEventKindV1::ApprovalRecorded { step_id }
            }
            WorkflowRunCommandV1::RecordDenial { step_id } => {
                if !self
                    .steps
                    .get(&step_id)
                    .ok_or(WorkflowStateError::UnknownStep)?
                    .approval_requested
                {
                    return Err(WorkflowStateError::ApprovalNotRequested);
                }
                WorkflowRunEventKindV1::DenialRecorded { step_id }
            }
            WorkflowRunCommandV1::Pause => WorkflowRunEventKindV1::Paused,
            WorkflowRunCommandV1::Resume => WorkflowRunEventKindV1::Resumed,
            WorkflowRunCommandV1::RequestCancellation => {
                WorkflowRunEventKindV1::CancellationRequested
            }
            WorkflowRunCommandV1::ReconcileStepCancelled { step_id } => {
                WorkflowRunEventKindV1::StepReconciledCancelled { step_id }
            }
            WorkflowRunCommandV1::Restart => WorkflowRunEventKindV1::Restarted {
                authority_epoch: self
                    .authority_epoch
                    .checked_add(1)
                    .ok_or(WorkflowStateError::AggregateVersionOverflow)?,
            },
        };
        self.apply(&WorkflowRunEventV1 {
            run_id: self.run_id.clone(),
            aggregate_version: self.aggregate_version.next()?,
            command_id: context.command_id,
            input_digest: context.input_digest,
            occurred_at: context.occurred_at,
            event,
        })
    }

    pub fn apply(&self, event: &WorkflowRunEventV1) -> Result<Self, WorkflowStateError> {
        self.validate_envelope(event)?;
        if self.status.is_terminal() {
            return Err(WorkflowStateError::InvalidTransition);
        }
        let mut next = self.clone();
        match event.event() {
            WorkflowRunEventKindV1::Started { .. } => {
                return Err(WorkflowStateError::InvalidTransition);
            }
            WorkflowRunEventKindV1::StepStarted { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Ready)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Running;
            }
            WorkflowRunEventKindV1::StepSucceeded { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Running)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Succeeded;
                next.release_dependents();
                if next
                    .steps
                    .values()
                    .all(|step| step.status == WorkflowStepStatusV1::Succeeded)
                {
                    next.status = WorkflowRunStatusV1::Succeeded;
                }
            }
            WorkflowRunEventKindV1::StepFailed { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::Running)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Failed;
                next.status = WorkflowRunStatusV1::Failed;
            }
            WorkflowRunEventKindV1::ApprovalRequested { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::AwaitingApproval)?;
                let step = next.step_mut(step_id)?;
                if step.approval_requested {
                    return Err(WorkflowStateError::InvalidTransition);
                }
                step.approval_requested = true;
            }
            WorkflowRunEventKindV1::ApprovalRecorded { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::AwaitingApproval)?;
                let step = next.step_mut(step_id)?;
                if !step.approval_requested {
                    return Err(WorkflowStateError::ApprovalNotRequested);
                }
                step.status = WorkflowStepStatusV1::Ready;
            }
            WorkflowRunEventKindV1::DenialRecorded { step_id } => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.require_step_status(step_id, WorkflowStepStatusV1::AwaitingApproval)?;
                if !next.step_mut(step_id)?.approval_requested {
                    return Err(WorkflowStateError::ApprovalNotRequested);
                }
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Failed;
                next.status = WorkflowRunStatusV1::Failed;
            }
            WorkflowRunEventKindV1::Paused => {
                next.require_run_status(WorkflowRunStatusV1::Running)?;
                next.status = WorkflowRunStatusV1::Paused;
            }
            WorkflowRunEventKindV1::Resumed => {
                next.require_run_status(WorkflowRunStatusV1::Paused)?;
                next.status = WorkflowRunStatusV1::Running;
            }
            WorkflowRunEventKindV1::CancellationRequested => {
                if !matches!(
                    next.status,
                    WorkflowRunStatusV1::Running | WorkflowRunStatusV1::Paused
                ) {
                    return Err(WorkflowStateError::InvalidTransition);
                }
                next.status = WorkflowRunStatusV1::Cancelling;
                for step in next.steps.values_mut() {
                    step.status = match step.status {
                        WorkflowStepStatusV1::Running | WorkflowStepStatusV1::ReconcileRequired => {
                            WorkflowStepStatusV1::ReconcileRequired
                        }
                        WorkflowStepStatusV1::Succeeded | WorkflowStepStatusV1::Failed => {
                            step.status
                        }
                        _ => WorkflowStepStatusV1::Cancelled,
                    };
                }
                if !next
                    .steps
                    .values()
                    .any(|step| step.status == WorkflowStepStatusV1::ReconcileRequired)
                {
                    next.status = WorkflowRunStatusV1::Cancelled;
                }
            }
            WorkflowRunEventKindV1::StepReconciledCancelled { step_id } => {
                next.require_step_status(step_id, WorkflowStepStatusV1::ReconcileRequired)?;
                next.step_mut(step_id)?.status = WorkflowStepStatusV1::Cancelled;
                if next.status == WorkflowRunStatusV1::Cancelling
                    && !next
                        .steps
                        .values()
                        .any(|step| step.status == WorkflowStepStatusV1::ReconcileRequired)
                {
                    next.status = WorkflowRunStatusV1::Cancelled;
                }
            }
            WorkflowRunEventKindV1::Restarted { authority_epoch } => {
                if !matches!(
                    next.status,
                    WorkflowRunStatusV1::Running | WorkflowRunStatusV1::Paused
                ) || *authority_epoch
                    != next
                        .authority_epoch
                        .checked_add(1)
                        .ok_or(WorkflowStateError::AggregateVersionOverflow)?
                {
                    return Err(WorkflowStateError::InvalidTransition);
                }
                next.authority_epoch = *authority_epoch;
                next.status = WorkflowRunStatusV1::Running;
                for step in next.steps.values_mut() {
                    if step.status == WorkflowStepStatusV1::Running {
                        step.status = WorkflowStepStatusV1::ReconcileRequired;
                    }
                }
            }
        }
        next.aggregate_version = event.aggregate_version();
        next.history.push(event.clone());
        Ok(next)
    }

    fn validate_envelope(&self, event: &WorkflowRunEventV1) -> Result<(), WorkflowStateError> {
        if event.run_id() != &self.run_id {
            return Err(WorkflowStateError::MixedAggregate);
        }
        if event.aggregate_version() != self.aggregate_version.next()? {
            return Err(WorkflowStateError::NonContiguousVersion);
        }
        if event.occurred_at() < self.last_occurred_at()? {
            return Err(WorkflowStateError::NonMonotonicTime);
        }
        if self
            .history
            .iter()
            .any(|admitted| admitted.command_id() == event.command_id())
        {
            return Err(WorkflowStateError::DuplicateCommand);
        }
        Ok(())
    }

    fn require_run_status(&self, status: WorkflowRunStatusV1) -> Result<(), WorkflowStateError> {
        if self.status != status {
            return Err(WorkflowStateError::InvalidTransition);
        }
        Ok(())
    }

    fn require_step_status(
        &self,
        step_id: &WorkflowStepId,
        status: WorkflowStepStatusV1,
    ) -> Result<(), WorkflowStateError> {
        if self
            .steps
            .get(step_id)
            .ok_or(WorkflowStateError::UnknownStep)?
            .status
            != status
        {
            return Err(WorkflowStateError::InvalidTransition);
        }
        Ok(())
    }

    fn step_mut(
        &mut self,
        step_id: &WorkflowStepId,
    ) -> Result<&mut WorkflowStepProjectionV1, WorkflowStateError> {
        self.steps
            .get_mut(step_id)
            .ok_or(WorkflowStateError::UnknownStep)
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
            let ready = definition_step.predecessors.iter().all(|predecessor| {
                self.steps.get(predecessor).map(|step| step.status)
                    == Some(WorkflowStepStatusV1::Succeeded)
            });
            if ready {
                let status = Self::released_status(&self.approval_steps, &definition_step.step_id);
                if let Some(step) = self.steps.get_mut(&definition_step.step_id) {
                    step.status = status;
                }
            }
        }
    }

    fn released_status(
        approval_steps: &BTreeSet<WorkflowStepId>,
        step_id: &WorkflowStepId,
    ) -> WorkflowStepStatusV1 {
        if approval_steps.contains(step_id) {
            WorkflowStepStatusV1::AwaitingApproval
        } else {
            WorkflowStepStatusV1::Ready
        }
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub const fn status(&self) -> WorkflowRunStatusV1 {
        self.status
    }

    pub const fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    pub const fn authority_epoch(&self) -> u64 {
        self.authority_epoch
    }

    pub const fn aggregate_version(&self) -> WorkflowAggregateVersionV1 {
        self.aggregate_version
    }

    pub fn step_status(&self, step_id: &WorkflowStepId) -> Option<WorkflowStepStatusV1> {
        self.steps
            .get(step_id)
            .map(WorkflowStepProjectionV1::status)
    }

    pub fn steps(&self) -> &BTreeMap<WorkflowStepId, WorkflowStepProjectionV1> {
        &self.steps
    }

    pub fn history(&self) -> &[WorkflowRunEventV1] {
        &self.history
    }

    fn last_occurred_at(&self) -> Result<UtcMicros, WorkflowStateError> {
        self.history
            .last()
            .map(WorkflowRunEventV1::occurred_at)
            .ok_or(WorkflowStateError::EmptyHistory)
    }
}
