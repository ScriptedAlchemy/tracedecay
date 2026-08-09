//! Canonical execution-attempt, lease, cancellation, recovery, and terminal contracts for Work.

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    AttemptId, CommitId, ManifestDigest, ProjectId, ProposalId, ProviderId, RefId, RepositoryId,
    RunId, RuntimeEvidenceRef, TaskId, UtcMicros, WorkArtifactId, WorkCancellationRequestId,
    WorkExecutionLimits, WorkExecutionSnapshot, WorkGraphVersionV1, WorkLeaseId,
    WorkProductEventSequenceV1, WorkProductGraphV1, WorkProductSourceWatermarkV1,
    WorkProviderRouteId, WorkflowOperationRef, WorktreeId,
};

pub const MAX_WORK_ATTEMPT_ARTIFACTS: usize = 256;
pub const MAX_WORK_INSTRUCTIONS_BYTES: usize = 65_536;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkRuntimeContractError {
    #[error("Work fence epoch must be non-zero")]
    InvalidFenceEpoch,
    #[error("Work attempt progress must have a non-zero total and completed must not exceed total")]
    InvalidProgress,
    #[error("Work artifact byte length must be non-zero")]
    InvalidArtifact,
    #[error("Work attempt carries too many artifacts")]
    TooManyArtifacts,
    #[error("Work attempt repeats an artifact identity")]
    DuplicateArtifact,
    #[error("Work cancellation timestamps are not monotonic")]
    InvalidCancellationOrder,
    #[error("Work attempt state is inconsistent with its attached evidence")]
    InconsistentAttemptState,
    #[error("Work attempt transition is not permitted")]
    InvalidAttemptTransition,
    #[error("Work attempt transition changed immutable identity")]
    MixedAttemptIdentity,
    #[error("Work attempt lease identity or fence epoch regressed")]
    StaleLeaseFence,
    #[error("Work attempt recovery cannot reference itself")]
    SelfRecovery,
    #[error("Work attempt does not match its Work projection")]
    ProjectionMismatch,
    #[error("Work execution has not been admitted")]
    ExecutionNotAdmitted,
    #[error("Work execution envelope is invalid")]
    InvalidExecutionEnvelope,
    #[error("Work execution configuration snapshot is invalid")]
    InvalidExecutionSnapshot,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
#[schemars(title = "WorkFenceEpochV1")]
pub struct WorkFenceEpochV1(u64);

impl WorkFenceEpochV1 {
    pub fn new(value: u64) -> Result<Self, WorkRuntimeContractError> {
        if value == 0 {
            return Err(WorkRuntimeContractError::InvalidFenceEpoch);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for WorkFenceEpochV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptIdentityV1 {
    task_id: TaskId,
    run_id: RunId,
    attempt_id: AttemptId,
}

impl WorkAttemptIdentityV1 {
    pub fn new(
        task_id: TaskId,
        run_id: RunId,
        attempt_id: AttemptId,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self {
            task_id,
            run_id,
            attempt_id,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkLeaseFenceV1 {
    lease_id: WorkLeaseId,
    epoch: WorkFenceEpochV1,
}

impl WorkLeaseFenceV1 {
    pub fn new(
        lease_id: WorkLeaseId,
        epoch: WorkFenceEpochV1,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self { lease_id, epoch })
    }

    pub fn lease_id(&self) -> &WorkLeaseId {
        &self.lease_id
    }

    pub const fn epoch(&self) -> WorkFenceEpochV1 {
        self.epoch
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProviderRouteV1 {
    provider_id: ProviderId,
    route_id: WorkProviderRouteId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptProjectionBindingV1 {
    graph_version: WorkGraphVersionV1,
    /// Exact immutable product event that verified this graph version.
    event_sequence: WorkProductEventSequenceV1,
    /// Source-frontier identity preserved from the verified product graph.
    source_watermark: WorkProductSourceWatermarkV1,
    /// Digest of the graph recovered and verified at admission.
    recovered_graph_digest: ManifestDigest,
    /// Exact accepted proposal the attempt was admitted against. A superseded
    /// or cleared proposal is a different binding, not a compatible refresh.
    accepted_proposal: ProposalId,
}

impl WorkAttemptProjectionBindingV1 {
    pub fn new(
        graph_version: WorkGraphVersionV1,
        event_sequence: WorkProductEventSequenceV1,
        source_watermark: WorkProductSourceWatermarkV1,
        recovered_graph_digest: ManifestDigest,
        accepted_proposal: ProposalId,
    ) -> Result<Self, WorkRuntimeContractError> {
        recovered_graph_digest
            .validate()
            .map_err(|_| WorkRuntimeContractError::ProjectionMismatch)?;
        Ok(Self {
            graph_version,
            event_sequence,
            source_watermark,
            recovered_graph_digest,
            accepted_proposal,
        })
    }

    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub const fn event_sequence(&self) -> WorkProductEventSequenceV1 {
        self.event_sequence
    }

    pub fn source_watermark(&self) -> &WorkProductSourceWatermarkV1 {
        &self.source_watermark
    }

    pub fn recovered_graph_digest(&self) -> &ManifestDigest {
        &self.recovered_graph_digest
    }

    pub fn accepted_proposal(&self) -> &ProposalId {
        &self.accepted_proposal
    }
}

impl WorkProviderRouteV1 {
    pub fn new(
        provider_id: ProviderId,
        route_id: WorkProviderRouteId,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self {
            provider_id,
            route_id,
        })
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn route_id(&self) -> &WorkProviderRouteId {
        &self.route_id
    }
}

/// Provider protocol selected by the pinned Work configuration snapshot.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProviderBackendV1 {
    ClaudeCodeCli,
    CodexAppServer,
    CodexCli,
}

/// Effect semantics admitted for one provider attempt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkEffectStateV1 {
    Observational,
    Intercepted,
    CompoundNonRepeatable,
}

/// Immutable stream and artifact ceilings reserved before provider startup.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutionBudgetV1 {
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    max_protocol_bytes: u64,
}

impl WorkExecutionBudgetV1 {
    pub fn new(
        max_stdout_bytes: u64,
        max_stderr_bytes: u64,
        max_protocol_bytes: u64,
    ) -> Result<Self, WorkRuntimeContractError> {
        if max_stdout_bytes == 0 || max_stderr_bytes == 0 || max_protocol_bytes == 0 {
            return Err(WorkRuntimeContractError::InvalidExecutionEnvelope);
        }
        Ok(Self {
            max_stdout_bytes,
            max_stderr_bytes,
            max_protocol_bytes,
        })
    }

    pub const fn max_stdout_bytes(self) -> u64 {
        self.max_stdout_bytes
    }

    pub const fn max_stderr_bytes(self) -> u64 {
        self.max_stderr_bytes
    }

    pub const fn max_protocol_bytes(self) -> u64 {
        self.max_protocol_bytes
    }

    pub const fn from_limits(limits: WorkExecutionLimits) -> Self {
        Self {
            max_stdout_bytes: limits.max_stdout_bytes(),
            max_stderr_bytes: limits.max_stderr_bytes(),
            max_protocol_bytes: limits.max_protocol_bytes(),
        }
    }
}

impl<'de> Deserialize<'de> for WorkExecutionBudgetV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            max_stdout_bytes: u64,
            max_stderr_bytes: u64,
            max_protocol_bytes: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.max_stdout_bytes,
            wire.max_stderr_bytes,
            wire.max_protocol_bytes,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Exact immutable provider admission attached to the durable Work attempt.
///
/// Callers name typed route and scope facts, never argv, environment entries,
/// or executable paths. The daemon resolves the registered executable only
/// after this envelope has been persisted and admitted to the canonical queue.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecutionEnvelopeV1 {
    attempt_identity: WorkAttemptIdentityV1,
    projection_binding: WorkAttemptProjectionBindingV1,
    operation: WorkflowOperationRef,
    execution_snapshot: WorkExecutionSnapshot,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    worktree_root: String,
    reference: Option<RefId>,
    commit: CommitId,
    /// Exact provider instructions admitted with this attempt. The adapter
    /// delivers these bytes verbatim on the provider's typed input channel;
    /// they are never interpolated into argv or an ambient prompt source.
    instructions: String,
    cancellation_generation: u64,
    effect_state: WorkEffectStateV1,
}

impl WorkExecutionEnvelopeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        attempt_identity: WorkAttemptIdentityV1,
        projection_binding: WorkAttemptProjectionBindingV1,
        operation: WorkflowOperationRef,
        execution_snapshot: WorkExecutionSnapshot,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        worktree_root: String,
        reference: Option<RefId>,
        commit: CommitId,
        instructions: String,
        cancellation_generation: u64,
        effect_state: WorkEffectStateV1,
    ) -> Result<Self, WorkRuntimeContractError> {
        if worktree_root.len() > 4_096
            || !Path::new(&worktree_root).is_absolute()
            || worktree_root.contains('\0')
            || instructions.is_empty()
            || instructions.len() > MAX_WORK_INSTRUCTIONS_BYTES
            || instructions.contains('\0')
            || cancellation_generation == 0
        {
            return Err(WorkRuntimeContractError::InvalidExecutionEnvelope);
        }
        Ok(Self {
            attempt_identity,
            projection_binding,
            operation,
            execution_snapshot,
            project_id,
            repository_id,
            worktree_id,
            worktree_root,
            reference,
            commit,
            instructions,
            cancellation_generation,
            effect_state,
        })
    }

    pub fn attempt_identity(&self) -> &WorkAttemptIdentityV1 {
        &self.attempt_identity
    }

    pub fn projection_binding(&self) -> &WorkAttemptProjectionBindingV1 {
        &self.projection_binding
    }

    /// Whether two envelopes carry the same *caller-supplied* admission
    /// content: identity, operation, pinned execution snapshot, scope,
    /// worktree/reference/commit, instructions, cancellation generation, and
    /// effect state.
    ///
    /// The projection binding is deliberately excluded. It is server-derived
    /// state pinned when the attempt was admitted, and its generation,
    /// sequence, and work version necessarily advance as *any* Work event
    /// lands on the authority — including the attempt's own terminal runtime
    /// evidence. Comparing it against a freshly recomputed binding would
    /// therefore classify a byte-identical replay of the same admission as a
    /// content conflict as soon as the attempt reported its own outcome, so
    /// idempotent replay would be unreachable in practice. The proposal the
    /// attempt was admitted against is the part of the binding that is real
    /// admission authority, and callers check it separately through
    /// [`WorkAttemptProjectionBindingV1::accepted_proposal`].
    pub fn same_admission_content(&self, other: &Self) -> bool {
        self.attempt_identity == other.attempt_identity
            && self.operation == other.operation
            && self.execution_snapshot == other.execution_snapshot
            && self.project_id == other.project_id
            && self.repository_id == other.repository_id
            && self.worktree_id == other.worktree_id
            && self.worktree_root == other.worktree_root
            && self.reference == other.reference
            && self.commit == other.commit
            && self.instructions == other.instructions
            && self.cancellation_generation == other.cancellation_generation
            && self.effect_state == other.effect_state
    }

    pub fn operation(&self) -> &WorkflowOperationRef {
        &self.operation
    }

    pub fn route(&self) -> &WorkProviderRouteV1 {
        self.execution_snapshot.route()
    }

    pub const fn backend(&self) -> WorkProviderBackendV1 {
        self.execution_snapshot.backend()
    }

    pub fn model(&self) -> &str {
        self.execution_snapshot.model()
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        self.execution_snapshot.effective_behavior_digest()
    }

    pub fn execution_snapshot(&self) -> &WorkExecutionSnapshot {
        &self.execution_snapshot
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub fn worktree_root(&self) -> &str {
        &self.worktree_root
    }

    pub fn reference(&self) -> Option<&RefId> {
        self.reference.as_ref()
    }

    pub fn commit(&self) -> &CommitId {
        &self.commit
    }

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    pub const fn deadline(&self) -> UtcMicros {
        self.execution_snapshot.deadline()
    }

    pub const fn cancellation_generation(&self) -> u64 {
        self.cancellation_generation
    }

    pub const fn budget(&self) -> WorkExecutionBudgetV1 {
        WorkExecutionBudgetV1::from_limits(self.execution_snapshot.limits())
    }

    pub const fn effect_state(&self) -> WorkEffectStateV1 {
        self.effect_state
    }

    fn validate_attempt(
        &self,
        identity: &WorkAttemptIdentityV1,
        projection_binding: &WorkAttemptProjectionBindingV1,
        requested_route: &WorkProviderRouteV1,
    ) -> Result<(), WorkRuntimeContractError> {
        if &self.attempt_identity != identity
            || &self.projection_binding != projection_binding
            || self.execution_snapshot.route() != requested_route
        {
            return Err(WorkRuntimeContractError::InvalidExecutionEnvelope);
        }
        Ok(())
    }
}

impl WorkProviderBackendV1 {
    pub(crate) fn provider_id(self) -> &'static ProviderId {
        static CLAUDE: std::sync::OnceLock<ProviderId> = std::sync::OnceLock::new();
        static CODEX_APP_SERVER: std::sync::OnceLock<ProviderId> = std::sync::OnceLock::new();
        static CODEX_CLI: std::sync::OnceLock<ProviderId> = std::sync::OnceLock::new();
        match self {
            Self::ClaudeCodeCli => CLAUDE.get_or_init(|| {
                ProviderId::new("provider.work.claude-code-cli")
                    .expect("static Claude Work provider ID")
            }),
            Self::CodexAppServer => CODEX_APP_SERVER.get_or_init(|| {
                ProviderId::new("provider.work.codex-app-server")
                    .expect("static Codex app-server Work provider ID")
            }),
            Self::CodexCli => CODEX_CLI.get_or_init(|| {
                ProviderId::new("provider.work.codex-cli")
                    .expect("static Codex CLI Work provider ID")
            }),
        }
    }

    pub(crate) const fn protocol(self) -> crate::WorkProviderProtocol {
        match self {
            Self::ClaudeCodeCli => crate::WorkProviderProtocol::ClaudeStreamJson,
            Self::CodexAppServer => crate::WorkProviderProtocol::CodexAppServerJsonRpc,
            Self::CodexCli => crate::WorkProviderProtocol::CodexExecJson,
        }
    }
}

impl<'de> Deserialize<'de> for WorkExecutionEnvelopeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            attempt_identity: WorkAttemptIdentityV1,
            projection_binding: WorkAttemptProjectionBindingV1,
            operation: WorkflowOperationRef,
            execution_snapshot: WorkExecutionSnapshot,
            project_id: ProjectId,
            repository_id: RepositoryId,
            worktree_id: WorktreeId,
            worktree_root: String,
            reference: Option<RefId>,
            commit: CommitId,
            instructions: String,
            cancellation_generation: u64,
            effect_state: WorkEffectStateV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.attempt_identity,
            wire.projection_binding,
            wire.operation,
            wire.execution_snapshot,
            wire.project_id,
            wire.repository_id,
            wire.worktree_id,
            wire.worktree_root,
            wire.reference,
            wire.commit,
            wire.instructions,
            wire.cancellation_generation,
            wire.effect_state,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptProgressV1 {
    completed: u64,
    total: u64,
}

impl WorkAttemptProgressV1 {
    pub fn new(completed: u64, total: u64) -> Result<Self, WorkRuntimeContractError> {
        if total == 0 || completed > total {
            return Err(WorkRuntimeContractError::InvalidProgress);
        }
        Ok(Self { completed, total })
    }

    pub const fn completed(self) -> u64 {
        self.completed
    }

    pub const fn total(self) -> u64 {
        self.total
    }
}

impl<'de> Deserialize<'de> for WorkAttemptProgressV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            completed: u64,
            total: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.completed, wire.total).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkArtifactRefV1 {
    artifact_id: WorkArtifactId,
    digest: ManifestDigest,
    byte_length: u64,
}

impl WorkArtifactRefV1 {
    pub fn new(
        artifact_id: WorkArtifactId,
        digest: ManifestDigest,
        byte_length: u64,
    ) -> Result<Self, WorkRuntimeContractError> {
        if byte_length == 0 {
            return Err(WorkRuntimeContractError::InvalidArtifact);
        }
        Ok(Self {
            artifact_id,
            digest,
            byte_length,
        })
    }

    pub fn artifact_id(&self) -> &WorkArtifactId {
        &self.artifact_id
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }

    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

impl<'de> Deserialize<'de> for WorkArtifactRefV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            artifact_id: WorkArtifactId,
            digest: ManifestDigest,
            byte_length: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.artifact_id, wire.digest, wire.byte_length).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCancellationRequestV1 {
    request_id: WorkCancellationRequestId,
    requested_at: UtcMicros,
}

impl WorkCancellationRequestV1 {
    pub fn new(
        request_id: WorkCancellationRequestId,
        requested_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self {
            request_id,
            requested_at,
        })
    }

    pub fn request_id(&self) -> &WorkCancellationRequestId {
        &self.request_id
    }

    pub const fn requested_at(&self) -> UtcMicros {
        self.requested_at
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCancellationAcknowledgementV1 {
    request: WorkCancellationRequestV1,
    acknowledged_at: UtcMicros,
}

impl WorkCancellationAcknowledgementV1 {
    pub fn new(
        request: WorkCancellationRequestV1,
        acknowledged_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        if acknowledged_at < request.requested_at {
            return Err(WorkRuntimeContractError::InvalidCancellationOrder);
        }
        Ok(Self {
            request,
            acknowledged_at,
        })
    }

    pub fn request(&self) -> &WorkCancellationRequestV1 {
        &self.request
    }

    pub const fn acknowledged_at(&self) -> UtcMicros {
        self.acknowledged_at
    }
}

impl<'de> Deserialize<'de> for WorkCancellationAcknowledgementV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            request: WorkCancellationRequestV1,
            acknowledged_at: UtcMicros,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.request, wire.acknowledged_at).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCancellationEscalationV1 {
    acknowledgement: WorkCancellationAcknowledgementV1,
    escalated_at: UtcMicros,
}

impl WorkCancellationEscalationV1 {
    pub fn new(
        acknowledgement: WorkCancellationAcknowledgementV1,
        escalated_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        if escalated_at < acknowledgement.acknowledged_at {
            return Err(WorkRuntimeContractError::InvalidCancellationOrder);
        }
        Ok(Self {
            acknowledgement,
            escalated_at,
        })
    }

    pub fn acknowledgement(&self) -> &WorkCancellationAcknowledgementV1 {
        &self.acknowledgement
    }

    pub const fn escalated_at(&self) -> UtcMicros {
        self.escalated_at
    }
}

impl<'de> Deserialize<'de> for WorkCancellationEscalationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            acknowledgement: WorkCancellationAcknowledgementV1,
            escalated_at: UtcMicros,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.acknowledgement, wire.escalated_at).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum WorkCancellationStateV1 {
    None,
    Requested(WorkCancellationRequestV1),
    Acknowledged(WorkCancellationAcknowledgementV1),
    Escalated(WorkCancellationEscalationV1),
}

impl WorkCancellationStateV1 {
    fn request(&self) -> Option<&WorkCancellationRequestV1> {
        match self {
            Self::None => None,
            Self::Requested(request) => Some(request),
            Self::Acknowledged(acknowledgement) => Some(acknowledgement.request()),
            Self::Escalated(escalation) => Some(escalation.acknowledgement().request()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkRestartReasonV1 {
    LeaseLost,
    ProviderUnavailable,
    ProcessLost,
    FailureObserved,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkRecoveryStateV1 {
    Fresh,
    Resumed {
        source_attempt_id: AttemptId,
        checkpoint: Option<WorkArtifactRefV1>,
    },
    Restarted {
        source_attempt_id: AttemptId,
        reason: WorkRestartReasonV1,
    },
    /// The attempt cannot continue and must be recovered. A first attempt
    /// lost before it ever resumed anything has no predecessor, so the source
    /// is absent rather than pointing at the attempt itself.
    RecoveryRequired {
        #[serde(default)]
        source_attempt_id: Option<AttemptId>,
        reason: WorkRestartReasonV1,
    },
}

impl WorkRecoveryStateV1 {
    /// The predecessor attempt this recovery state resumes from, if any.
    pub fn source_attempt_id(&self) -> Option<&AttemptId> {
        match self {
            Self::Fresh => None,
            Self::Resumed {
                source_attempt_id, ..
            }
            | Self::Restarted {
                source_attempt_id, ..
            } => Some(source_attempt_id),
            Self::RecoveryRequired {
                source_attempt_id, ..
            } => source_attempt_id.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkAttemptStateV1 {
    Leased,
    Running,
    CancellationRequested,
    CancellationAcknowledged,
    CancellationEscalated,
    RecoveryRequired,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

impl WorkAttemptStateV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkTerminalEvidenceV1 {
    Succeeded {
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    Failed {
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    TimedOut {
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    Cancelled {
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
}

impl WorkTerminalEvidenceV1 {
    pub fn succeeded(
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self::Succeeded {
            evidence_digest,
            observed_at,
        })
    }

    pub fn failed(
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self::Failed {
            evidence_digest,
            observed_at,
        })
    }

    pub fn timed_out(
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self::TimedOut {
            evidence_digest,
            observed_at,
        })
    }

    pub fn cancelled(
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self::Cancelled {
            evidence_digest,
            observed_at,
        })
    }

    pub fn runtime_evidence_ref(
        &self,
        run_id: RunId,
    ) -> Result<RuntimeEvidenceRef, WorkRuntimeContractError> {
        let evidence_digest = match self {
            Self::Succeeded {
                evidence_digest, ..
            }
            | Self::Failed {
                evidence_digest, ..
            }
            | Self::TimedOut {
                evidence_digest, ..
            }
            | Self::Cancelled {
                evidence_digest, ..
            } => evidence_digest.clone(),
        };
        RuntimeEvidenceRef::new(run_id, evidence_digest, true)
            .map_err(|_| WorkRuntimeContractError::InconsistentAttemptState)
    }

    /// The durable instant at which the provider terminal was observed.
    ///
    /// Run-control closes an open blocked interval at this owner fact rather
    /// than reading a new clock while replaying the attempt's terminal CAS.
    pub const fn observed_at(&self) -> UtcMicros {
        match self {
            Self::Succeeded { observed_at, .. }
            | Self::Failed { observed_at, .. }
            | Self::TimedOut { observed_at, .. }
            | Self::Cancelled { observed_at, .. } => *observed_at,
        }
    }

    fn matches_state(&self, state: WorkAttemptStateV1) -> bool {
        matches!(
            (self, state),
            (Self::Succeeded { .. }, WorkAttemptStateV1::Succeeded)
                | (Self::Failed { .. }, WorkAttemptStateV1::Failed)
                | (Self::TimedOut { .. }, WorkAttemptStateV1::TimedOut)
                | (Self::Cancelled { .. }, WorkAttemptStateV1::Cancelled)
        )
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkAttemptV1 {
    identity: WorkAttemptIdentityV1,
    projection_binding: WorkAttemptProjectionBindingV1,
    execution: WorkExecutionEnvelopeV1,
    lease: WorkLeaseFenceV1,
    state: WorkAttemptStateV1,
    progress: Option<WorkAttemptProgressV1>,
    artifacts: Vec<WorkArtifactRefV1>,
    cancellation: WorkCancellationStateV1,
    recovery: WorkRecoveryStateV1,
    requested_route: WorkProviderRouteV1,
    actual_route: Option<WorkProviderRouteV1>,
    terminal: Option<WorkTerminalEvidenceV1>,
}

impl WorkAttemptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: WorkAttemptIdentityV1,
        projection_binding: WorkAttemptProjectionBindingV1,
        execution: WorkExecutionEnvelopeV1,
        lease: WorkLeaseFenceV1,
        state: WorkAttemptStateV1,
        progress: Option<WorkAttemptProgressV1>,
        mut artifacts: Vec<WorkArtifactRefV1>,
        cancellation: WorkCancellationStateV1,
        recovery: WorkRecoveryStateV1,
        requested_route: WorkProviderRouteV1,
        actual_route: Option<WorkProviderRouteV1>,
        terminal: Option<WorkTerminalEvidenceV1>,
    ) -> Result<Self, WorkRuntimeContractError> {
        canonicalize_artifacts(&mut artifacts)?;
        execution.validate_attempt(&identity, &projection_binding, &requested_route)?;
        let attempt = Self {
            identity,
            projection_binding,
            execution,
            lease,
            state,
            progress,
            artifacts,
            cancellation,
            recovery,
            requested_route,
            actual_route,
            terminal,
        };
        attempt.validate_shape()?;
        Ok(attempt)
    }

    pub fn identity(&self) -> &WorkAttemptIdentityV1 {
        &self.identity
    }

    pub fn projection_binding(&self) -> &WorkAttemptProjectionBindingV1 {
        &self.projection_binding
    }

    pub fn execution(&self) -> &WorkExecutionEnvelopeV1 {
        &self.execution
    }

    pub fn lease(&self) -> &WorkLeaseFenceV1 {
        &self.lease
    }

    pub const fn state(&self) -> WorkAttemptStateV1 {
        self.state
    }

    pub fn progress(&self) -> Option<WorkAttemptProgressV1> {
        self.progress
    }

    pub fn artifacts(&self) -> &[WorkArtifactRefV1] {
        &self.artifacts
    }

    pub fn cancellation(&self) -> &WorkCancellationStateV1 {
        &self.cancellation
    }

    pub fn recovery(&self) -> &WorkRecoveryStateV1 {
        &self.recovery
    }

    pub fn requested_route(&self) -> &WorkProviderRouteV1 {
        &self.requested_route
    }

    pub fn actual_route(&self) -> Option<&WorkProviderRouteV1> {
        self.actual_route.as_ref()
    }

    pub fn terminal(&self) -> Option<&WorkTerminalEvidenceV1> {
        self.terminal.as_ref()
    }

    pub const fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub fn validate_graph_admission(
        &self,
        graph: &WorkProductGraphV1,
    ) -> Result<(), WorkRuntimeContractError> {
        let item = graph
            .item(self.identity.task_id())
            .ok_or(WorkRuntimeContractError::ProjectionMismatch)?;
        if self.projection_binding.graph_version() != graph.version()
            || item.accepted_proposal() != Some(self.projection_binding.accepted_proposal())
        {
            return Err(WorkRuntimeContractError::ProjectionMismatch);
        }
        if !item.is_execution_admitted() || !item.accepted_attempts().contains(self.identity()) {
            return Err(WorkRuntimeContractError::ExecutionNotAdmitted);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn transition(
        &self,
        state: WorkAttemptStateV1,
        progress: Option<WorkAttemptProgressV1>,
        artifacts: Vec<WorkArtifactRefV1>,
        cancellation: WorkCancellationStateV1,
        recovery: WorkRecoveryStateV1,
        actual_route: Option<WorkProviderRouteV1>,
        terminal: Option<WorkTerminalEvidenceV1>,
        lease: WorkLeaseFenceV1,
    ) -> Result<Self, WorkRuntimeContractError> {
        if lease.lease_id != self.lease.lease_id || lease.epoch < self.lease.epoch {
            return Err(WorkRuntimeContractError::StaleLeaseFence);
        }
        if !valid_transition(self.state, state) {
            return Err(WorkRuntimeContractError::InvalidAttemptTransition);
        }
        if progress_regresses(self.progress, progress)
            || self
                .artifacts
                .iter()
                .any(|existing| !artifacts.contains(existing))
            || !cancellation_continues(&self.cancellation, &cancellation)
        {
            return Err(WorkRuntimeContractError::InvalidAttemptTransition);
        }
        Self::new(
            self.identity.clone(),
            self.projection_binding.clone(),
            self.execution.clone(),
            lease,
            state,
            progress,
            artifacts,
            cancellation,
            recovery,
            self.requested_route.clone(),
            actual_route,
            terminal,
        )
    }

    fn validate_shape(&self) -> Result<(), WorkRuntimeContractError> {
        if self.recovery.source_attempt_id() == Some(self.identity.attempt_id()) {
            return Err(WorkRuntimeContractError::SelfRecovery);
        }
        let valid = match self.state {
            WorkAttemptStateV1::Leased => {
                self.actual_route.is_none()
                    && self.progress.is_none()
                    && matches!(self.cancellation, WorkCancellationStateV1::None)
                    && matches!(self.recovery, WorkRecoveryStateV1::Fresh)
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::Running => {
                self.actual_route.is_some()
                    && matches!(self.cancellation, WorkCancellationStateV1::None)
                    && !matches!(self.recovery, WorkRecoveryStateV1::RecoveryRequired { .. })
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::CancellationRequested => {
                matches!(self.cancellation, WorkCancellationStateV1::Requested(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::CancellationAcknowledged => {
                matches!(self.cancellation, WorkCancellationStateV1::Acknowledged(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::CancellationEscalated => {
                matches!(self.cancellation, WorkCancellationStateV1::Escalated(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::RecoveryRequired => {
                matches!(self.recovery, WorkRecoveryStateV1::RecoveryRequired { .. })
                    && matches!(self.cancellation, WorkCancellationStateV1::None)
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::Succeeded
            | WorkAttemptStateV1::Failed
            | WorkAttemptStateV1::TimedOut => {
                self.actual_route.is_some()
                    && self
                        .terminal
                        .as_ref()
                        .is_some_and(|terminal| terminal.matches_state(self.state))
            }
            WorkAttemptStateV1::Cancelled => {
                matches!(
                    self.cancellation,
                    WorkCancellationStateV1::Acknowledged(_)
                        | WorkCancellationStateV1::Escalated(_)
                ) && self
                    .terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.matches_state(self.state))
            }
        };
        if !valid {
            return Err(WorkRuntimeContractError::InconsistentAttemptState);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for WorkAttemptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            identity: WorkAttemptIdentityV1,
            projection_binding: WorkAttemptProjectionBindingV1,
            execution: WorkExecutionEnvelopeV1,
            lease: WorkLeaseFenceV1,
            state: WorkAttemptStateV1,
            progress: Option<WorkAttemptProgressV1>,
            artifacts: Vec<WorkArtifactRefV1>,
            cancellation: WorkCancellationStateV1,
            recovery: WorkRecoveryStateV1,
            requested_route: WorkProviderRouteV1,
            actual_route: Option<WorkProviderRouteV1>,
            terminal: Option<WorkTerminalEvidenceV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.identity,
            wire.projection_binding,
            wire.execution,
            wire.lease,
            wire.state,
            wire.progress,
            wire.artifacts,
            wire.cancellation,
            wire.recovery,
            wire.requested_route,
            wire.actual_route,
            wire.terminal,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn canonicalize_artifacts(
    artifacts: &mut [WorkArtifactRefV1],
) -> Result<(), WorkRuntimeContractError> {
    if artifacts.len() > MAX_WORK_ATTEMPT_ARTIFACTS {
        return Err(WorkRuntimeContractError::TooManyArtifacts);
    }
    artifacts.sort_by(|left, right| left.artifact_id().cmp(right.artifact_id()));
    let mut ids = BTreeSet::new();
    if artifacts
        .iter()
        .any(|artifact| !ids.insert(artifact.artifact_id().clone()))
    {
        return Err(WorkRuntimeContractError::DuplicateArtifact);
    }
    Ok(())
}

fn valid_transition(from: WorkAttemptStateV1, to: WorkAttemptStateV1) -> bool {
    use WorkAttemptStateV1::{
        CancellationAcknowledged, CancellationEscalated, CancellationRequested, Cancelled, Failed,
        Leased, RecoveryRequired, Running, Succeeded, TimedOut,
    };
    matches!(
        (from, to),
        (Leased, Running | CancellationRequested | RecoveryRequired)
            | (
                Running,
                Running | CancellationRequested | RecoveryRequired | Succeeded | Failed | TimedOut
            )
            | (
                CancellationRequested,
                CancellationAcknowledged | CancellationEscalated | Cancelled
            )
            | (CancellationAcknowledged, CancellationEscalated | Cancelled)
            | (CancellationEscalated, Cancelled | Failed)
            | (RecoveryRequired, Running | CancellationRequested | Failed)
    )
}

fn progress_regresses(
    previous: Option<WorkAttemptProgressV1>,
    next: Option<WorkAttemptProgressV1>,
) -> bool {
    match (previous, next) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(previous), Some(next)) => {
            previous.total() != next.total() || next.completed() < previous.completed()
        }
    }
}

fn cancellation_continues(
    previous: &WorkCancellationStateV1,
    next: &WorkCancellationStateV1,
) -> bool {
    match previous.request() {
        None => true,
        Some(previous_request) => next.request() == Some(previous_request),
    }
}
