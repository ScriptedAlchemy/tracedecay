//! Application authority for event-journaled workflow runs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, RunId, WorkArtifactRefV1, WorkAuthority, WorkCommandId, WorkflowDefinition,
    WorkflowDefinitionId, WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext,
    WorkflowRunProjection, WorkflowRunStateError, canonical_text::canonical_framed_sha256,
};

/// Maximum number of workflow histories rebuilt by one restart-recovery read.
pub const WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1: usize = 32;

use crate::workflow_provider::WorkflowProviderRegistration;

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

    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError>;

    fn active_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
        let mut projections = self.projections()?;
        projections.sort_by(|left, right| left.run_id().as_str().cmp(right.run_id().as_str()));
        let mut candidates = projections
            .into_iter()
            .filter(|projection| {
                after.is_none_or(|cursor| {
                    projection.run_id().as_str() > cursor.after_run_id.as_str()
                })
            })
            .take(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 + 1)
            .collect::<Vec<_>>();
        let continuation = (candidates.len() > WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1).then(|| {
            WorkflowActiveRunRecoveryCursorV1 {
                after_run_id: candidates[WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 - 1]
                    .run_id()
                    .clone(),
            }
        });
        candidates.truncate(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1);
        candidates.retain(|projection| {
            !projection.status().is_terminal()
                && projection
                    .fan_out_plans()
                    .values()
                    .all(|plan| &plan.authority == authority)
        });
        Ok(WorkflowActiveRunRecoveryPageV1 {
            projections: candidates,
            continuation,
        })
    }

    fn fan_out_binding(
        &self,
        identity: &tracedecay_domain::WorkAttemptIdentityV1,
    ) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
        let mut binding = None;
        for projection in self.projections()? {
            for plan in projection.fan_out_plans().values() {
                if plan
                    .children
                    .iter()
                    .any(|child| &child.attempt_identity == identity)
                {
                    let candidate = WorkflowFanOutAttemptBindingV1 {
                        run_id: projection.run_id().clone(),
                        step_id: plan.step_id.clone(),
                        plan_digest: plan.plan_digest.clone(),
                    };
                    if binding
                        .as_ref()
                        .is_some_and(|existing| existing != &candidate)
                    {
                        return Err(WorkflowRunStorageError::InvalidHistory);
                    }
                    binding = Some(candidate);
                }
            }
        }
        Ok(binding)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowActiveRunRecoveryCursorV1 {
    pub after_run_id: RunId,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowActiveRunRecoveryPageV1 {
    pub projections: Vec<WorkflowRunProjection>,
    pub continuation: Option<WorkflowActiveRunRecoveryCursorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFanOutAttemptBindingV1 {
    pub run_id: RunId,
    pub step_id: tracedecay_domain::WorkflowStepId,
    pub plan_digest: ManifestDigest,
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
        self.admit_with_fan_out(run_id, definition, admission, Vec::new(), context)
    }

    pub fn admit_with_fan_out(
        &self,
        run_id: RunId,
        definition: WorkflowDefinition,
        admission: WorkflowAdmissionSnapshot,
        fan_out_plans: Vec<tracedecay_domain::WorkflowFanOutPlanV1>,
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
        let event = WorkflowRunEvent::admitted_with_fan_out(
            run_id,
            definition,
            admission.topology_digest,
            admission.provider_registry_digest,
            fan_out_plans,
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

/// Starts (admits) a journaled workflow run from an active definition.
///
/// The daemon derives every admission digest itself: the definition's own
/// pinned policy/configuration/catalog digests are checked against the live
/// environment, the topology digest comes from the evaluated topology policy,
/// and the provider registry digest is computed from this registration — the
/// caller never supplies a digest the runtime must trust.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunStartRequest {
    pub run_id: RunId,
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    pub provider: WorkflowProviderRegistration,
    pub fan_out: Option<crate::workflow_runtime::WorkflowFanOutStartV1>,
    pub command_id: WorkCommandId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunPauseRequest {
    pub run_id: RunId,
    pub expected_sequence: u64,
    pub command_id: WorkCommandId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunResumeRequest {
    pub run_id: RunId,
    pub expected_sequence: u64,
    pub command_id: WorkCommandId,
}

/// Requests cooperative cancellation as a durable typed transition; the run
/// settles to `Cancelled` when the runtime reconciles in-flight steps.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunCancelRequest {
    pub run_id: RunId,
    pub expected_sequence: u64,
    pub command_id: WorkCommandId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRunGetRequest {
    pub run_id: RunId,
}
