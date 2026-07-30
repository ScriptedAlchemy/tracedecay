//! Canonical execution-attempt, lease, cancellation, recovery, and terminal contracts for Work.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    AttemptId, ManifestDigest, ProjectionGenerationId, ProviderId, RunId, RuntimeEvidenceRef,
    TaskId, UtcMicros, WorkArtifactId, WorkCancellationRequestId, WorkLeaseId, WorkProjection,
    WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkProviderRouteId, WorkVersion,
};

pub const MAX_WORK_ATTEMPT_ARTIFACTS: usize = 256;

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
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
    work_version: WorkVersion,
}

impl WorkAttemptProjectionBindingV1 {
    pub fn new(
        generation_id: ProjectionGenerationId,
        sequence: WorkProjectionSequenceV1,
        work_version: WorkVersion,
    ) -> Result<Self, WorkRuntimeContractError> {
        Ok(Self {
            generation_id,
            sequence,
            work_version,
        })
    }

    pub fn generation_id(&self) -> &ProjectionGenerationId {
        &self.generation_id
    }

    pub const fn sequence(&self) -> WorkProjectionSequenceV1 {
        self.sequence
    }

    pub const fn work_version(&self) -> WorkVersion {
        self.work_version
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
    CheckpointRejected,
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
    RecoveryRequired {
        source_attempt_id: AttemptId,
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
            }
            | Self::RecoveryRequired {
                source_attempt_id, ..
            } => Some(source_attempt_id),
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
    Cancelled,
}

impl WorkAttemptStateV1 {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
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
            | Self::Cancelled {
                evidence_digest, ..
            } => evidence_digest.clone(),
        };
        RuntimeEvidenceRef::new(run_id, evidence_digest, true)
            .map_err(|_| WorkRuntimeContractError::InconsistentAttemptState)
    }

    fn matches_state(&self, state: WorkAttemptStateV1) -> bool {
        matches!(
            (self, state),
            (Self::Succeeded { .. }, WorkAttemptStateV1::Succeeded)
                | (Self::Failed { .. }, WorkAttemptStateV1::Failed)
                | (Self::Cancelled { .. }, WorkAttemptStateV1::Cancelled)
        )
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WorkAttemptV1 {
    identity: WorkAttemptIdentityV1,
    projection_binding: WorkAttemptProjectionBindingV1,
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
        let attempt = Self {
            identity,
            projection_binding,
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

    pub fn validate_projection(
        &self,
        projection: &WorkProjection,
    ) -> Result<(), WorkRuntimeContractError> {
        if self.identity.task_id() != projection.task_id()
            || self.projection_binding.work_version() > projection.version()
        {
            return Err(WorkRuntimeContractError::ProjectionMismatch);
        }
        if !projection.is_execution_admitted() {
            return Err(WorkRuntimeContractError::ExecutionNotAdmitted);
        }
        Ok(())
    }

    pub fn validate_snapshot(
        &self,
        snapshot: &WorkProjectionSnapshotV1,
    ) -> Result<(), WorkRuntimeContractError> {
        if self.projection_binding.generation_id() != snapshot.generation_id()
            || self.projection_binding.sequence() > snapshot.sequence()
        {
            return Err(WorkRuntimeContractError::ProjectionMismatch);
        }
        let projection = snapshot
            .projections()
            .iter()
            .find(|projection| projection.task_id() == self.identity.task_id())
            .ok_or(WorkRuntimeContractError::ProjectionMismatch)?;
        self.validate_projection(projection)
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
                self.actual_route.is_some()
                    && matches!(self.cancellation, WorkCancellationStateV1::Requested(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::CancellationAcknowledged => {
                self.actual_route.is_some()
                    && matches!(self.cancellation, WorkCancellationStateV1::Acknowledged(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::CancellationEscalated => {
                self.actual_route.is_some()
                    && matches!(self.cancellation, WorkCancellationStateV1::Escalated(_))
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::RecoveryRequired => {
                matches!(self.recovery, WorkRecoveryStateV1::RecoveryRequired { .. })
                    && matches!(self.cancellation, WorkCancellationStateV1::None)
                    && self.terminal.is_none()
            }
            WorkAttemptStateV1::Succeeded | WorkAttemptStateV1::Failed => {
                self.actual_route.is_some()
                    && self
                        .terminal
                        .as_ref()
                        .is_some_and(|terminal| terminal.matches_state(self.state))
            }
            WorkAttemptStateV1::Cancelled => {
                self.actual_route.is_some()
                    && matches!(
                        self.cancellation,
                        WorkCancellationStateV1::Acknowledged(_)
                            | WorkCancellationStateV1::Escalated(_)
                    )
                    && self
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
        Leased, RecoveryRequired, Running, Succeeded,
    };
    matches!(
        (from, to),
        (Leased, Running | RecoveryRequired)
            | (
                Running,
                Running | CancellationRequested | RecoveryRequired | Succeeded | Failed
            )
            | (
                CancellationRequested,
                CancellationAcknowledged | CancellationEscalated | Cancelled
            )
            | (CancellationAcknowledged, CancellationEscalated | Cancelled)
            | (CancellationEscalated, Cancelled | Failed)
            | (RecoveryRequired, Running | Failed)
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
