//! Application authority for event-journaled workflow runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkArtifactRefV1, WorkflowDefinitionId, WorkflowDefinitionV1,
    WorkflowOperationRef, WorkflowRunCommandV1, WorkflowRunEventContextV1, WorkflowRunEventV1,
    WorkflowRunProjectionV1, WorkflowRunStateError, WorkflowStepId, WorkflowStepOutputV1,
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
    fn load(&self, run_id: &RunId) -> Result<Vec<WorkflowRunEventV1>, WorkflowRunStorageError>;

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
        let event =
            WorkflowRunEventV1::admitted(run_id, definition, admission.topology_digest, context)?;
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
    pub inputs: Vec<WorkArtifactRefV1>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStepExecutionError {
    #[error("workflow step execution failed")]
    Failed,
    #[error("workflow step execution authority is unavailable")]
    Unavailable,
}

pub trait WorkflowStepExecutionPort: Send + Sync {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequestV1,
    ) -> Result<Vec<WorkflowStepOutputV1>, WorkflowStepExecutionError>;
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
        };
        let outputs = match self.executor.execute(&request) {
            Ok(outputs) => outputs,
            Err(_) => {
                return self.fail_started_step(&started, step_id, contexts.failed);
            }
        };
        match started.next_event(
            WorkflowRunCommandV1::CompleteStep {
                step_id: step_id.clone(),
                outputs,
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
            Err(WorkflowRunStateError::InvalidStepOutputs) => {
                self.fail_started_step(&started, step_id, contexts.failed)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn fail_started_step(
        &self,
        started: &WorkflowRunProjectionV1,
        step_id: &WorkflowStepId,
        context: WorkflowRunEventContextV1,
    ) -> Result<WorkflowStepExecutionOutcomeV1, WorkflowStepExecutionServiceError> {
        let failed = started.next_event(
            WorkflowRunCommandV1::FailStep {
                step_id: step_id.clone(),
            },
            context,
        )?;
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
