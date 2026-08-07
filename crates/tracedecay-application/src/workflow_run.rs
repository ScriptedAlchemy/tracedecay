//! Application authority for event-journaled workflow runs.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkArtifactRefV1, WorkflowDefinition, WorkflowDefinitionId,
    WorkflowOperationRef, WorkflowPlacementReceipt, WorkflowRunCommand, WorkflowRunEvent,
    WorkflowRunEventContext, WorkflowRunProjection, WorkflowRunStateError,
    WorkflowStepEffectReceipt, WorkflowStepId, WorkflowStepInput, WorkflowStepOutput,
    canonical_sha256, canonical_text::canonical_framed_sha256,
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

/// Upper bound on one durable workflow artifact payload.
///
/// Artifacts enter only declared bounded channels; the bound is enforced both
/// when a payload is persisted and when it is hydrated back, so an
/// out-of-contract row can never silently re-enter execution.
pub const MAX_WORKFLOW_ARTIFACT_PAYLOAD_BYTES: u64 = 4 * 1024 * 1024;

const WORKFLOW_ARTIFACT_PAYLOAD_DIGEST_DOMAIN: &[u8] =
    b"tracedecay.application.workflow-artifact-payload.v1";

/// The canonical content digest a [`WorkArtifactRefV1`] must declare for a
/// workflow artifact payload.
///
/// The framed hash always yields a canonical `sha256:`-tagged digest, so the
/// only failure is the (unreachable) digest-shape rejection, reported typed.
pub fn workflow_artifact_payload_digest(
    bytes: &[u8],
) -> Result<ManifestDigest, WorkflowArtifactStoreError> {
    ManifestDigest::new(format!(
        "sha256:{}",
        canonical_framed_sha256(WORKFLOW_ARTIFACT_PAYLOAD_DIGEST_DOMAIN, &[bytes])
    ))
    .map_err(|_| WorkflowArtifactStoreError::DigestMismatch)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkflowArtifactStoreError {
    #[error("workflow artifact payload does not match its declared reference")]
    DigestMismatch,
    #[error("workflow artifact payload exceeds the admitted byte bound")]
    Oversized,
    #[error("workflow artifact payload conflicts with an already persisted payload")]
    PayloadConflict,
    #[error("workflow artifact payload is absent from the durable store")]
    Missing,
    #[error("workflow artifact authority is unavailable")]
    Unavailable,
}

/// One artifact payload verified against its declared reference.
///
/// Construction is the only way to obtain a value: the byte length and the
/// canonical content digest must both match the reference, so a hydrated or
/// about-to-persist payload is always evidence, never trust.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowArtifactPayload {
    artifact: WorkArtifactRefV1,
    bytes: Vec<u8>,
}

impl WorkflowArtifactPayload {
    pub fn new(
        artifact: WorkArtifactRefV1,
        bytes: Vec<u8>,
    ) -> Result<Self, WorkflowArtifactStoreError> {
        if artifact.byte_length() > MAX_WORKFLOW_ARTIFACT_PAYLOAD_BYTES {
            return Err(WorkflowArtifactStoreError::Oversized);
        }
        if bytes.len() as u64 != artifact.byte_length()
            || &workflow_artifact_payload_digest(&bytes)? != artifact.digest()
        {
            return Err(WorkflowArtifactStoreError::DigestMismatch);
        }
        Ok(Self { artifact, bytes })
    }

    pub fn artifact(&self) -> &WorkArtifactRefV1 {
        &self.artifact
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl<'de> Deserialize<'de> for WorkflowArtifactPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            artifact: WorkArtifactRefV1,
            bytes: Vec<u8>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.artifact, wire.bytes).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowArtifactPersistOutcome {
    Persisted,
    Replayed,
}

/// Durable digest-addressed workflow artifact payload store.
pub trait WorkflowArtifactStorePort: Send + Sync {
    fn persist(
        &self,
        payload: &WorkflowArtifactPayload,
    ) -> Result<WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError>;

    fn load(
        &self,
        artifact: &WorkArtifactRefV1,
    ) -> Result<WorkflowArtifactPayload, WorkflowArtifactStoreError>;
}

/// One resolved step input with every pinned artifact payload hydrated.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowHydratedInput {
    pub input: WorkflowStepInput,
    pub payloads: Vec<WorkflowArtifactPayload>,
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
    /// Payload bytes for every artifact the outputs declare, persisted by the
    /// runtime before the completion event is journaled.
    pub artifact_payloads: Vec<WorkflowArtifactPayload>,
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
        hydrated: &[WorkflowHydratedInput],
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
    #[error("workflow step input artifacts cannot be hydrated: {0}")]
    InputArtifacts(#[source] WorkflowArtifactStoreError),
    #[error(transparent)]
    State(#[from] WorkflowRunStateError),
    #[error(transparent)]
    Storage(#[from] WorkflowRunStorageError),
}

pub struct WorkflowStepExecutionService<S, E, A> {
    storage: S,
    executor: E,
    artifacts: A,
}

impl<S, E, A> WorkflowStepExecutionService<S, E, A>
where
    S: WorkflowRunStoragePort,
    E: WorkflowStepExecutionPort,
    A: WorkflowArtifactStorePort,
{
    pub const fn new(storage: S, executor: E, artifacts: A) -> Self {
        Self {
            storage,
            executor,
            artifacts,
        }
    }

    /// Hydrates every pinned artifact of the resolved inputs, refusing typed
    /// before any run state changes when a payload is absent or corrupt.
    fn hydrate_inputs(
        &self,
        inputs: &[WorkflowStepInput],
    ) -> Result<Vec<WorkflowHydratedInput>, WorkflowStepExecutionServiceError> {
        inputs
            .iter()
            .map(|input| {
                let payloads = input
                    .artifacts()
                    .iter()
                    .map(|artifact| self.artifacts.load(artifact.artifact()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(WorkflowStepExecutionServiceError::InputArtifacts)?;
                Ok(WorkflowHydratedInput {
                    input: input.clone(),
                    payloads,
                })
            })
            .collect()
    }

    /// Persists every declared output payload before the completion event is
    /// journaled. `None` means the result does not correspond to its own
    /// declared artifacts (or the store refused) — a protocol failure.
    fn persist_output_payloads(&self, result: &WorkflowStepExecutionResult) -> Option<()> {
        let declared = result
            .outputs
            .iter()
            .flat_map(WorkflowStepOutput::artifacts)
            .map(|artifact| artifact.artifact())
            .map(|artifact| (artifact.digest(), artifact.artifact_id(), artifact.byte_length()))
            .collect::<BTreeSet<_>>();
        let supplied = result
            .artifact_payloads
            .iter()
            .map(WorkflowArtifactPayload::artifact)
            .map(|artifact| (artifact.digest(), artifact.artifact_id(), artifact.byte_length()))
            .collect::<BTreeSet<_>>();
        if declared != supplied || result.artifact_payloads.len() != supplied.len() {
            return None;
        }
        for payload in &result.artifact_payloads {
            self.artifacts.persist(payload).ok()?;
        }
        Some(())
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
        // Hydration failures refuse before any run state changes: a step that
        // cannot see its pinned inputs was never started.
        let hydrated = self.hydrate_inputs(&inputs)?;
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
        let result = match self.executor.execute(&request, &hydrated) {
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
        // Payloads persist before the completion event: a crash between the
        // two leaves only idempotent digest-addressed rows behind, never a
        // journaled completion whose artifacts cannot hydrate.
        if self.persist_output_payloads(&result).is_none() {
            return self.fail_started_step(
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
            );
        }
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
