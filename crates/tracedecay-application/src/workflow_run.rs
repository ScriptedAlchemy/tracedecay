//! Application authority for event-journaled workflow runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef,
    WorkflowPlacementReceipt, WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext,
    WorkflowRunProjection, WorkflowRunStateError, WorkflowStepEffectReceipt, WorkflowStepId,
    WorkflowStepInput, WorkflowStepOutput, canonical_sha256,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowRunStorageError {
    #[error("workflow run was not found")]
    NotFound,
    #[error("workflow run sequence changed")]
    VersionConflict,
    #[error("workflow run command identity was reused with different input")]
    IdempotencyConflict,
    #[error("workflow run history is invalid")]
    InvalidHistory,
    #[error("workflow run storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunAppendRequest {
    pub expected_sequence: Option<u64>,
    pub event: WorkflowRunEvent,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "projection")]
pub enum WorkflowRunAppendOutcome {
    Appended(WorkflowRunProjection),
    Replayed(WorkflowRunProjection),
}

impl WorkflowRunAppendOutcome {
    pub fn into_projection(self) -> WorkflowRunProjection {
        match self {
            Self::Appended(projection) | Self::Replayed(projection) => projection,
        }
    }
}

pub trait WorkflowRunStoragePort: Send + Sync {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError>;

    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError>;
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkflowRunServiceError {
    #[error("workflow policy digest is stale")]
    PolicyDigestMismatch,
    #[error("workflow configuration digest is stale")]
    ConfigurationDigestMismatch,
    #[error("workflow catalog digest is stale")]
    CatalogDigestMismatch,
    #[error(transparent)]
    State(#[from] WorkflowRunStateError),
    #[error(transparent)]
    Storage(#[from] WorkflowRunStorageError),
}

pub struct WorkflowRunService<P> {
    storage: P,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowAdmissionSnapshot {
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub catalog_digest: ManifestDigest,
    pub topology_digest: ManifestDigest,
    pub provider_registry_digest: ManifestDigest,
}

impl<P> WorkflowRunService<P>
where
    P: WorkflowRunStoragePort,
{
    pub const fn new(storage: P) -> Self {
        Self { storage }
    }

    pub fn admit(
        &self,
        run_id: RunId,
        definition: WorkflowDefinition,
        admission: WorkflowAdmissionSnapshot,
        context: WorkflowRunEventContext,
    ) -> Result<WorkflowRunProjection, WorkflowRunServiceError> {
        if definition.pinned_policy_digest() != &admission.policy_digest {
            return Err(WorkflowRunServiceError::PolicyDigestMismatch);
        }
        if definition.pinned_configuration_digest() != &admission.configuration_digest {
            return Err(WorkflowRunServiceError::ConfigurationDigestMismatch);
        }
        if definition.pinned_catalog_digest() != &admission.catalog_digest {
            return Err(WorkflowRunServiceError::CatalogDigestMismatch);
        }
        let event = WorkflowRunEvent::admitted(
            run_id,
            definition,
            admission.topology_digest,
            admission.provider_registry_digest,
            context,
        )?;
        Ok(self
            .storage
            .append(&WorkflowRunAppendRequest {
                expected_sequence: None,
                event,
            })?
            .into_projection())
    }

    pub fn apply(
        &self,
        run_id: &RunId,
        expected_sequence: u64,
        command: WorkflowRunCommand,
        context: WorkflowRunEventContext,
    ) -> Result<WorkflowRunProjection, WorkflowRunServiceError> {
        let projection = self.storage.projection(run_id)?;
        if projection.sequence() != expected_sequence {
            return Err(WorkflowRunStorageError::VersionConflict.into());
        }
        let event = projection.next_event(command, context)?;
        Ok(self
            .storage
            .append(&WorkflowRunAppendRequest {
                expected_sequence: Some(expected_sequence),
                event,
            })?
            .into_projection())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepEventContexts {
    pub started: WorkflowRunEventContext,
    pub completed: WorkflowRunEventContext,
    pub failed: WorkflowRunEventContext,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepExecutionRequest {
    pub run_id: RunId,
    pub definition_id: WorkflowDefinitionId,
    pub definition_version: u64,
    pub step_id: WorkflowStepId,
    pub operation: WorkflowOperationRef,
    pub inputs: Vec<WorkflowStepInput>,
    pub placement: WorkflowPlacementReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepExecutionResult {
    pub outputs: Vec<WorkflowStepOutput>,
    pub effect_receipt: WorkflowStepEffectReceipt,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStepExecutionError {
    #[error("workflow step execution failed")]
    Failed {
        effect_receipt: Box<WorkflowStepEffectReceipt>,
    },
    #[error("workflow step execution authority is unavailable")]
    Unavailable {
        effect_receipt: Box<WorkflowStepEffectReceipt>,
    },
}

impl WorkflowStepExecutionError {
    pub fn into_effect_receipt(self) -> WorkflowStepEffectReceipt {
        match self {
            Self::Failed { effect_receipt } | Self::Unavailable { effect_receipt } => {
                *effect_receipt
            }
        }
    }
}

pub trait WorkflowStepExecutionPort: Send + Sync {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequest,
    ) -> Result<WorkflowStepExecutionResult, WorkflowStepExecutionError>;
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "projection")]
pub enum WorkflowStepExecutionOutcome {
    Succeeded(WorkflowRunProjection),
    Failed(WorkflowRunProjection),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkflowStepExecutionServiceError {
    #[error("workflow step is absent from the pinned definition")]
    StepNotFound,
    #[error(transparent)]
    State(#[from] WorkflowRunStateError),
    #[error(transparent)]
    Storage(#[from] WorkflowRunStorageError),
}

pub struct WorkflowStepExecutionService<S, E> {
    storage: S,
    executor: E,
}

impl<S, E> WorkflowStepExecutionService<S, E>
where
    S: WorkflowRunStoragePort,
    E: WorkflowStepExecutionPort,
{
    pub const fn new(storage: S, executor: E) -> Self {
        Self { storage, executor }
    }

    pub fn execute_ready_step(
        &self,
        run_id: &RunId,
        expected_sequence: u64,
        step_id: &WorkflowStepId,
        placement: WorkflowPlacementReceipt,
        contexts: WorkflowStepEventContexts,
    ) -> Result<WorkflowStepExecutionOutcome, WorkflowStepExecutionServiceError> {
        let projection = self.storage.projection(run_id)?;
        if projection.sequence() != expected_sequence {
            return Err(WorkflowRunStorageError::VersionConflict.into());
        }
        let definition_step = projection
            .definition()
            .steps()
            .iter()
            .find(|step| &step.step_id == step_id)
            .cloned()
            .ok_or(WorkflowStepExecutionServiceError::StepNotFound)?;
        let inputs = projection.resolved_inputs(step_id)?;
        let started_event = projection.next_event(
            WorkflowRunCommand::StartStep {
                step_id: step_id.clone(),
                placement: placement.clone(),
            },
            contexts.started,
        )?;
        let started = self
            .storage
            .append(&WorkflowRunAppendRequest {
                expected_sequence: Some(expected_sequence),
                event: started_event,
            })?
            .into_projection();
        let request = WorkflowStepExecutionRequest {
            run_id: run_id.clone(),
            definition_id: started.definition().definition_id().clone(),
            definition_version: started.definition().definition_version(),
            step_id: step_id.clone(),
            operation: definition_step.operation,
            inputs,
            placement,
        };
        let result = match self.executor.execute(&request) {
            Ok(result) => result,
            Err(error) => {
                return self.fail_started_step(
                    &started,
                    step_id,
                    Vec::new(),
                    error.into_effect_receipt(),
                    contexts.failed,
                );
            }
        };
        match started.next_event(
            WorkflowRunCommand::CompleteStep {
                step_id: step_id.clone(),
                outputs: result.outputs.clone(),
                effect_receipt: result.effect_receipt.clone(),
            },
            contexts.completed,
        ) {
            Ok(event) => Ok(WorkflowStepExecutionOutcome::Succeeded(
                self.storage
                    .append(&WorkflowRunAppendRequest {
                        expected_sequence: Some(started.sequence()),
                        event,
                    })?
                    .into_projection(),
            )),
            Err(
                WorkflowRunStateError::InvalidStepOutputs
                | WorkflowRunStateError::InvalidEffectReceipt,
            ) => self.fail_started_step(
                &started,
                step_id,
                Vec::new(),
                protocol_failure_receipt(
                    &started,
                    step_id,
                    &result.effect_receipt,
                    &result.outputs,
                )?,
                contexts.failed,
            ),
            Err(error) => Err(error.into()),
        }
    }

    fn fail_started_step(
        &self,
        started: &WorkflowRunProjection,
        step_id: &WorkflowStepId,
        outputs: Vec<WorkflowStepOutput>,
        effect_receipt: WorkflowStepEffectReceipt,
        context: WorkflowRunEventContext,
    ) -> Result<WorkflowStepExecutionOutcome, WorkflowStepExecutionServiceError> {
        let failed = match started.next_event(
            WorkflowRunCommand::FailStep {
                step_id: step_id.clone(),
                outputs: outputs.clone(),
                effect_receipt: effect_receipt.clone(),
            },
            context.clone(),
        ) {
            Ok(failed) => failed,
            Err(
                WorkflowRunStateError::InvalidStepOutputs
                | WorkflowRunStateError::InvalidEffectReceipt,
            ) => started.next_event(
                WorkflowRunCommand::FailStep {
                    step_id: step_id.clone(),
                    outputs: Vec::new(),
                    effect_receipt: protocol_failure_receipt(
                        started,
                        step_id,
                        &effect_receipt,
                        &outputs,
                    )?,
                },
                context,
            )?,
            Err(error) => return Err(error.into()),
        };
        Ok(WorkflowStepExecutionOutcome::Failed(
            self.storage
                .append(&WorkflowRunAppendRequest {
                    expected_sequence: Some(started.sequence()),
                    event: failed,
                })?
                .into_projection(),
        ))
    }
}

fn protocol_failure_receipt(
    started: &WorkflowRunProjection,
    step_id: &WorkflowStepId,
    observed_receipt: &WorkflowStepEffectReceipt,
    observed_outputs: &[WorkflowStepOutput],
) -> Result<WorkflowStepEffectReceipt, WorkflowStepExecutionServiceError> {
    let placement_digest = started
        .step(step_id)
        .and_then(|step| step.placement_receipt())
        .map(|placement| placement.placement_digest().clone())
        .ok_or(WorkflowRunStateError::InvalidPlacementReceipt)?;
    let effect_digest = canonical_sha256(&(
        "tracedecay.application.workflow-provider-protocol-failure.v1",
        observed_receipt,
        observed_outputs,
    ))
    .map_err(|_| WorkflowRunStateError::InvalidEffectReceipt)?;
    WorkflowStepEffectReceipt::new(
        started.run_id().clone(),
        step_id.clone(),
        placement_digest,
        tracedecay_domain::WorkflowStepEffectOutcome::Failed,
        effect_digest,
        &[],
    )
    .map_err(|_| WorkflowRunStateError::InvalidEffectReceipt.into())
}
