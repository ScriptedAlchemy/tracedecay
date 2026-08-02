//! Application authority for event-journaled workflow runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef,
    WorkflowPlacementReceiptV1, WorkflowRunCommandV1, WorkflowRunEventContextV1,
    WorkflowRunEventV1, WorkflowRunProjectionV1, WorkflowRunStateError,
    WorkflowStepEffectReceiptV1, WorkflowStepId, WorkflowStepInputV1, WorkflowStepOutputV1,
    canonical_sha256,
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
pub struct WorkflowRunAppendRequestV1 {
    pub expected_sequence: Option<u64>,
    pub event: WorkflowRunEventV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "projection")]
pub enum WorkflowRunAppendOutcomeV1 {
    Appended(WorkflowRunProjectionV1),
    Replayed(WorkflowRunProjectionV1),
}

impl WorkflowRunAppendOutcomeV1 {
    pub fn into_projection(self) -> WorkflowRunProjectionV1 {
        match self {
            Self::Appended(projection) | Self::Replayed(projection) => projection,
        }
    }
}

pub trait WorkflowRunStoragePort: Send + Sync {
    fn projection(
        &self,
        run_id: &RunId,
    ) -> Result<WorkflowRunProjectionV1, WorkflowRunStorageError>;

    fn append(
        &self,
        request: &WorkflowRunAppendRequestV1,
    ) -> Result<WorkflowRunAppendOutcomeV1, WorkflowRunStorageError>;
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
pub struct WorkflowAdmissionSnapshotV1 {
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
        definition: WorkflowDefinitionV1,
        admission: WorkflowAdmissionSnapshotV1,
        context: WorkflowRunEventContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowRunServiceError> {
        if definition.pinned_policy_digest() != &admission.policy_digest {
            return Err(WorkflowRunServiceError::PolicyDigestMismatch);
        }
        if definition.pinned_configuration_digest() != &admission.configuration_digest {
            return Err(WorkflowRunServiceError::ConfigurationDigestMismatch);
        }
        if definition.pinned_catalog_digest() != &admission.catalog_digest {
            return Err(WorkflowRunServiceError::CatalogDigestMismatch);
        }
        let event = WorkflowRunEventV1::admitted(
            run_id,
            definition,
            admission.topology_digest,
            admission.provider_registry_digest,
            context,
        )?;
        Ok(self
            .storage
            .append(&WorkflowRunAppendRequestV1 {
                expected_sequence: None,
                event,
            })?
            .into_projection())
    }

    pub fn apply(
        &self,
        run_id: &RunId,
        expected_sequence: u64,
        command: WorkflowRunCommandV1,
        context: WorkflowRunEventContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowRunServiceError> {
        let projection = self.storage.projection(run_id)?;
        if projection.sequence() != expected_sequence {
            return Err(WorkflowRunStorageError::VersionConflict.into());
        }
        let event = projection.next_event(command, context)?;
        Ok(self
            .storage
            .append(&WorkflowRunAppendRequestV1 {
                expected_sequence: Some(expected_sequence),
                event,
            })?
            .into_projection())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepEventContextsV1 {
    pub started: WorkflowRunEventContextV1,
    pub completed: WorkflowRunEventContextV1,
    pub failed: WorkflowRunEventContextV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepExecutionRequestV1 {
    pub run_id: RunId,
    pub definition_id: WorkflowDefinitionId,
    pub definition_version: u64,
    pub step_id: WorkflowStepId,
    pub operation: WorkflowOperationRef,
    pub inputs: Vec<WorkflowStepInputV1>,
    pub placement: WorkflowPlacementReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStepExecutionResultV1 {
    pub outputs: Vec<WorkflowStepOutputV1>,
    pub effect_receipt: WorkflowStepEffectReceiptV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStepExecutionError {
    #[error("workflow step execution failed")]
    Failed {
        effect_receipt: Box<WorkflowStepEffectReceiptV1>,
    },
    #[error("workflow step execution authority is unavailable")]
    Unavailable {
        effect_receipt: Box<WorkflowStepEffectReceiptV1>,
    },
}

impl WorkflowStepExecutionError {
    pub fn into_effect_receipt(self) -> WorkflowStepEffectReceiptV1 {
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
        request: &WorkflowStepExecutionRequestV1,
    ) -> Result<WorkflowStepExecutionResultV1, WorkflowStepExecutionError>;
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "projection")]
pub enum WorkflowStepExecutionOutcomeV1 {
    Succeeded(WorkflowRunProjectionV1),
    Failed(WorkflowRunProjectionV1),
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
        placement: WorkflowPlacementReceiptV1,
        contexts: WorkflowStepEventContextsV1,
    ) -> Result<WorkflowStepExecutionOutcomeV1, WorkflowStepExecutionServiceError> {
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
            WorkflowRunCommandV1::StartStep {
                step_id: step_id.clone(),
                placement: placement.clone(),
            },
            contexts.started,
        )?;
        let started = self
            .storage
            .append(&WorkflowRunAppendRequestV1 {
                expected_sequence: Some(expected_sequence),
                event: started_event,
            })?
            .into_projection();
        let request = WorkflowStepExecutionRequestV1 {
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
            WorkflowRunCommandV1::CompleteStep {
                step_id: step_id.clone(),
                outputs: result.outputs.clone(),
                effect_receipt: result.effect_receipt.clone(),
            },
            contexts.completed,
        ) {
            Ok(event) => Ok(WorkflowStepExecutionOutcomeV1::Succeeded(
                self.storage
                    .append(&WorkflowRunAppendRequestV1 {
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
        started: &WorkflowRunProjectionV1,
        step_id: &WorkflowStepId,
        outputs: Vec<WorkflowStepOutputV1>,
        effect_receipt: WorkflowStepEffectReceiptV1,
        context: WorkflowRunEventContextV1,
    ) -> Result<WorkflowStepExecutionOutcomeV1, WorkflowStepExecutionServiceError> {
        let failed = match started.next_event(
            WorkflowRunCommandV1::FailStep {
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
                WorkflowRunCommandV1::FailStep {
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
        Ok(WorkflowStepExecutionOutcomeV1::Failed(
            self.storage
                .append(&WorkflowRunAppendRequestV1 {
                    expected_sequence: Some(started.sequence()),
                    event: failed,
                })?
                .into_projection(),
        ))
    }
}

fn protocol_failure_receipt(
    started: &WorkflowRunProjectionV1,
    step_id: &WorkflowStepId,
    observed_receipt: &WorkflowStepEffectReceiptV1,
    observed_outputs: &[WorkflowStepOutputV1],
) -> Result<WorkflowStepEffectReceiptV1, WorkflowStepExecutionServiceError> {
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
    WorkflowStepEffectReceiptV1::new(
        started.run_id().clone(),
        step_id.clone(),
        placement_digest,
        tracedecay_domain::WorkflowStepEffectOutcomeV1::Failed,
        effect_digest,
        &[],
    )
    .map_err(|_| WorkflowRunStateError::InvalidEffectReceipt.into())
}
