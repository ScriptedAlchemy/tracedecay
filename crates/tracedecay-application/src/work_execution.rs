use std::fmt::Display;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    UtcMicros, WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAttemptProgressV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationAcknowledgementV1, WorkCancellationEscalationV1, WorkCancellationRequestV1,
    WorkCancellationStateV1, WorkExecutionEnvelopeV1, WorkLeaseFenceV1, WorkProjectionSnapshotV1,
    WorkProviderRouteV1, WorkRecoveryStateV1, WorkRestartReasonV1, WorkRuntimeContractError,
    WorkTerminalEvidenceV1,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptAcquireLeaseRequestV1 {
    pub snapshot: WorkProjectionSnapshotV1,
    pub identity: WorkAttemptIdentityV1,
    pub projection_binding: WorkAttemptProjectionBindingV1,
    pub execution: WorkExecutionEnvelopeV1,
    pub lease: WorkLeaseFenceV1,
    pub requested_route: WorkProviderRouteV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptRenewLeaseRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub expected: WorkLeaseFenceV1,
    pub replacement: WorkLeaseFenceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptStartRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub recovery: WorkRecoveryStateV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptPublishProgressRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub progress: WorkAttemptProgressV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptPublishArtifactRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub artifact: WorkArtifactRefV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptCancelRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub request: WorkCancellationRequestV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptRecoverRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub reason: WorkRestartReasonV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptTerminalizeRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub terminal: WorkTerminalEvidenceV1,
}

/// Seals the outcome the provider itself reported.
///
/// This carries no terminal: the caller does not get to say how the attempt
/// ended, because the digest would then be whatever the caller invented rather
/// than a hash of the evidence the provider produced. The runtime claims the
/// settlement, seals its evidence as an artifact, and derives the terminal.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptFinishRequestV1 {
    pub identity: WorkAttemptIdentityV1,
    pub lease: WorkLeaseFenceV1,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptResponseV1 {
    pub attempt: WorkAttemptV1,
}

impl From<WorkAttemptV1> for WorkAttemptResponseV1 {
    fn from(attempt: WorkAttemptV1) -> Self {
        Self { attempt }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkExecutionPersistenceError {
    Conflict,
    /// Projection generation or authority failed validation before any write.
    InvalidRequest,
    Unavailable(String),
}

impl Display for WorkExecutionPersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => formatter.write_str("work attempt changed concurrently"),
            Self::InvalidRequest => {
                formatter.write_str("work attempt projection generation or authority is invalid")
            }
            Self::Unavailable(message) => {
                write!(formatter, "work attempt persistence unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for WorkExecutionPersistenceError {}

pub trait WorkAttemptPersistencePort: Send + Sync {
    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionPersistenceError>;

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<(), WorkExecutionPersistenceError>;

    fn compare_and_swap(
        &self,
        authority: &WorkAuthority,
        expected: &WorkAttemptV1,
        replacement: &WorkAttemptV1,
    ) -> Result<(), WorkExecutionPersistenceError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkProviderExecutionError {
    Unavailable(String),
    Rejected(String),
}

impl Display for WorkProviderExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "work provider unavailable: {message}"),
            Self::Rejected(message) => {
                write!(formatter, "work provider rejected request: {message}")
            }
        }
    }
}

impl std::error::Error for WorkProviderExecutionError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkExecutionError {
    NotFound,
    AlreadyExists,
    StaleLease,
    TerminalConflict,
    Contract(WorkRuntimeContractError),
    Persistence(WorkExecutionPersistenceError),
    Provider(WorkProviderExecutionError),
}

impl Display for WorkExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("work attempt was not found"),
            Self::AlreadyExists => formatter.write_str("work attempt already exists"),
            Self::StaleLease => formatter.write_str("work attempt lease is stale"),
            Self::TerminalConflict => {
                formatter.write_str("work attempt has a different terminal outcome")
            }
            Self::Contract(error) => Display::fmt(error, formatter),
            Self::Persistence(error) => Display::fmt(error, formatter),
            Self::Provider(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for WorkExecutionError {}

impl From<WorkRuntimeContractError> for WorkExecutionError {
    fn from(error: WorkRuntimeContractError) -> Self {
        Self::Contract(error)
    }
}

impl From<WorkExecutionPersistenceError> for WorkExecutionError {
    fn from(error: WorkExecutionPersistenceError) -> Self {
        Self::Persistence(error)
    }
}

impl From<WorkProviderExecutionError> for WorkExecutionError {
    fn from(error: WorkProviderExecutionError) -> Self {
        Self::Provider(error)
    }
}

/// Records every durable Work attempt transition.
///
/// The service owns state only: it never reaches a provider, so a recorded
/// transition can never trail a side effect. Provider execution is admitted
/// from the durable intent by `WorkExecutionQueueV1`.
pub struct WorkExecutionService<S> {
    persistence: S,
}

impl<S> WorkExecutionService<S>
where
    S: WorkAttemptPersistencePort,
{
    pub const fn new(persistence: S) -> Self {
        Self { persistence }
    }

    pub fn acquire_lease(
        &self,
        authority: &WorkAuthority,
        request: WorkAttemptAcquireLeaseRequestV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let WorkAttemptAcquireLeaseRequestV1 {
            snapshot,
            identity,
            projection_binding,
            execution,
            lease,
            requested_route,
        } = request;
        let attempt = WorkAttemptV1::new(
            identity,
            projection_binding,
            execution,
            lease,
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            requested_route,
            None,
            None,
        )?;
        attempt.validate_snapshot(&snapshot)?;
        match self.persistence.insert(authority, &attempt) {
            Ok(()) => Ok(attempt),
            Err(WorkExecutionPersistenceError::Conflict) => {
                let existing = self
                    .persistence
                    .load(authority, attempt.identity())?
                    .ok_or(WorkExecutionError::AlreadyExists)?;
                if existing == attempt {
                    Ok(existing)
                } else {
                    Err(WorkExecutionError::AlreadyExists)
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn renew_lease(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        expected: &WorkLeaseFenceV1,
        replacement: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, expected)?;
        // A settled attempt has no execution left to hold, so raising its fence
        // would only let a caller keep asserting ownership of a closed attempt.
        if current.is_terminal() {
            return Err(WorkExecutionError::TerminalConflict);
        }
        if replacement.lease_id() != expected.lease_id() || replacement.epoch() <= expected.epoch()
        {
            return Err(WorkExecutionError::StaleLease);
        }
        let next = rebuild_attempt(&current, replacement)?;
        self.persistence
            .compare_and_swap(authority, &current, &next)?;
        Ok(next)
    }

    /// Records the durable intent to run an attempt on `route`.
    ///
    /// Replaying `start` for an already-running attempt re-records the same
    /// state, so a caller whose provider admission was refused can retry
    /// without a compensating rollback.
    pub fn start(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        recovery: WorkRecoveryStateV1,
        route: WorkProviderRouteV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let artifacts = current.artifacts().to_vec();
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::Running,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            recovery,
            Some(route),
            None,
            lease.clone(),
        )
    }

    pub fn publish_progress(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        progress: WorkAttemptProgressV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        if current.progress().is_some_and(|previous| {
            previous.total() != progress.total() || previous.completed() > progress.completed()
        }) {
            return Err(WorkRuntimeContractError::InvalidAttemptTransition.into());
        }
        let artifacts = current.artifacts().to_vec();
        let recovery = current.recovery().clone();
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::Running,
            Some(progress),
            artifacts,
            WorkCancellationStateV1::None,
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    pub fn publish_artifact(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        artifact: WorkArtifactRefV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let mut artifacts = current.artifacts().to_vec();
        let recovery = current.recovery().clone();
        if !artifacts.contains(&artifact) {
            artifacts.push(artifact);
        }
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::Running,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    /// Records the durable intent to cancel an attempt before any provider is
    /// asked to stop.
    pub fn request_cancellation(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        request: WorkCancellationRequestV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        // Replaying a cancellation is how a caller retries after a refused
        // provider stop, so the same request must be idempotent. A different
        // request would silently rewrite who asked and why, and the terminal
        // replay check would then answer against whichever landed last.
        match current.cancellation() {
            WorkCancellationStateV1::Requested(recorded) if recorded != &request => {
                return Err(WorkExecutionError::TerminalConflict);
            }
            WorkCancellationStateV1::Acknowledged(acknowledgement)
                if acknowledgement.request() != &request =>
            {
                return Err(WorkExecutionError::TerminalConflict);
            }
            _ => {}
        }
        let artifacts = current.artifacts().to_vec();
        let recovery = current.recovery().clone();
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::CancellationRequested,
            None,
            artifacts,
            WorkCancellationStateV1::Requested(request),
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    pub fn acknowledge_cancellation(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        acknowledgement: WorkCancellationAcknowledgementV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let artifacts = current.artifacts().to_vec();
        let recovery = current.recovery().clone();
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::CancellationAcknowledged,
            None,
            artifacts,
            WorkCancellationStateV1::Acknowledged(acknowledgement),
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    pub fn escalate_cancellation(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        escalation: WorkCancellationEscalationV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let artifacts = current.artifacts().to_vec();
        let recovery = current.recovery().clone();
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::CancellationEscalated,
            None,
            artifacts,
            WorkCancellationStateV1::Escalated(escalation),
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    pub fn require_recovery(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        reason: WorkRestartReasonV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let artifacts = current.artifacts().to_vec();
        let recovery = WorkRecoveryStateV1::RecoveryRequired {
            source_attempt_id: current.recovery().source_attempt_id().cloned(),
            reason,
        };
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::RecoveryRequired,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            recovery,
            None,
            None,
            lease.clone(),
        )
    }

    pub fn terminalize(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        terminal: WorkTerminalEvidenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        if current.is_terminal() {
            return if current.terminal() == Some(&terminal) {
                Ok(current)
            } else {
                Err(WorkExecutionError::TerminalConflict)
            };
        }
        let state = match terminal {
            WorkTerminalEvidenceV1::Succeeded { .. } => WorkAttemptStateV1::Succeeded,
            WorkTerminalEvidenceV1::Failed { .. } => WorkAttemptStateV1::Failed,
            WorkTerminalEvidenceV1::TimedOut { .. } => WorkAttemptStateV1::TimedOut,
            WorkTerminalEvidenceV1::Cancelled { .. } => WorkAttemptStateV1::Cancelled,
        };
        let artifacts = current.artifacts().to_vec();
        let cancellation = current.cancellation().clone();
        let recovery = current.recovery().clone();
        self.transition(
            authority,
            current,
            state,
            None,
            artifacts,
            cancellation,
            recovery,
            None,
            Some(terminal),
            lease.clone(),
        )
    }

    fn load_with_fence(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt = self
            .persistence
            .load(authority, identity)?
            .ok_or(WorkExecutionError::NotFound)?;
        if attempt.lease() != lease {
            return Err(WorkExecutionError::StaleLease);
        }
        Ok(attempt)
    }

    #[allow(clippy::too_many_arguments)]
    fn transition(
        &self,
        authority: &WorkAuthority,
        current: WorkAttemptV1,
        state: WorkAttemptStateV1,
        progress: Option<WorkAttemptProgressV1>,
        artifacts: Vec<WorkArtifactRefV1>,
        cancellation: WorkCancellationStateV1,
        recovery: WorkRecoveryStateV1,
        actual_route: Option<WorkProviderRouteV1>,
        terminal: Option<WorkTerminalEvidenceV1>,
        lease: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let progress = progress.or(current.progress());
        let actual_route = actual_route.or_else(|| current.actual_route().cloned());
        let next = if current.state() == state {
            WorkAttemptV1::new(
                current.identity().clone(),
                current.projection_binding().clone(),
                current.execution().clone(),
                lease,
                state,
                progress,
                artifacts,
                cancellation,
                recovery,
                current.requested_route().clone(),
                actual_route,
                terminal,
            )?
        } else {
            current.transition(
                state,
                progress,
                artifacts,
                cancellation,
                recovery,
                actual_route,
                terminal,
                lease,
            )?
        };
        self.persistence
            .compare_and_swap(authority, &current, &next)?;
        Ok(next)
    }
}

fn rebuild_attempt(
    attempt: &WorkAttemptV1,
    lease: WorkLeaseFenceV1,
) -> Result<WorkAttemptV1, WorkRuntimeContractError> {
    WorkAttemptV1::new(
        attempt.identity().clone(),
        attempt.projection_binding().clone(),
        attempt.execution().clone(),
        lease,
        attempt.state(),
        attempt.progress(),
        attempt.artifacts().to_vec(),
        attempt.cancellation().clone(),
        attempt.recovery().clone(),
        attempt.requested_route().clone(),
        attempt.actual_route().cloned(),
        attempt.terminal().cloned(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use tracedecay_domain::{
        ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId,
        ManifestDigest, ProjectId, ProjectionGenerationId, ProposalId, ProviderId, RefId,
        RepositoryId, RunId, TaskId, UtcMicros, WorkApprovalPolicy, WorkArtifactId,
        WorkCancellationRequestId, WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference,
        WorkExecutionLimits, WorkExecutionSnapshot, WorkExecutionSnapshotInput,
        WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy, WorkLeaseId,
        WorkProjectionSequenceV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
        WorkSandboxPolicy, WorkVersion, WorkflowOperationRef, WorktreeId,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn authority() -> WorkAuthority {
        WorkAuthority::new(
            id::<ProjectId>("project.work.execution"),
            id::<RepositoryId>("repository.work.execution"),
            id::<WorktreeId>("worktree.work.execution"),
            id::<ActorId>("actor.work.execution"),
            digest('f'),
        )
        .unwrap()
    }

    fn identity(value: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            id::<TaskId>("task.work.execution"),
            id::<RunId>("run.work.execution"),
            id::<AttemptId>(value),
        )
        .unwrap()
    }

    fn lease(epoch: u64) -> WorkLeaseFenceV1 {
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.work.execution"),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap()
    }

    fn route(value: &str) -> WorkProviderRouteV1 {
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.work.codex-app-server"),
            id::<WorkProviderRouteId>(value),
        )
        .unwrap()
    }

    fn execution_snapshot(route: WorkProviderRouteV1) -> WorkExecutionSnapshot {
        WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
            configuration_revision_id: id::<ConfigurationRevisionId>(
                "configuration-revision.work.execution",
            ),
            configuration_snapshot_id: id::<ConfigurationSnapshotId>(
                "configuration-snapshot.work.execution",
            ),
            effective_behavior_digest: digest('c'),
            resolution_provenance_digest: digest('d'),
            route,
            backend: WorkProviderBackendV1::CodexAppServer,
            protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
            model: "gpt-test".to_owned(),
            executable: WorkExecutableReference::new(
                "executable.codex.app-server".to_owned(),
                digest('e'),
            )
            .unwrap(),
            sandbox: WorkSandboxPolicy::Required,
            approval: WorkApprovalPolicy::Never,
            filesystem: WorkFilesystemPolicy::WorkspaceWrite,
            egress: WorkEgressPolicy::Deny,
            environment_allowlist: BTreeSet::new(),
            credential_references: BTreeSet::new(),
            limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
            deadline: UtcMicros(1_000_000),
            fallback: WorkFallbackTopology::Disabled,
            topology_policy_digest: digest('f'),
        })
        .unwrap()
    }

    fn execution_envelope(
        identity: WorkAttemptIdentityV1,
        projection_binding: WorkAttemptProjectionBindingV1,
        route: WorkProviderRouteV1,
    ) -> WorkExecutionEnvelopeV1 {
        WorkExecutionEnvelopeV1::new(
            identity,
            projection_binding,
            id::<WorkflowOperationRef>("operation.work.execute-provider"),
            execution_snapshot(route),
            id::<ProjectId>("project.work.execution"),
            id::<RepositoryId>("repository.work.execution"),
            id::<WorktreeId>("worktree.work.execution"),
            "/tmp/work-execution".to_owned(),
            Some(id::<RefId>("refs/heads/work-execution")),
            id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
            1,
            WorkEffectStateV1::Observational,
        )
        .unwrap()
    }

    fn leased_attempt(attempt_id: &str) -> WorkAttemptV1 {
        let identity = identity(attempt_id);
        let projection_binding = WorkAttemptProjectionBindingV1::new(
            id::<ProjectionGenerationId>("generation.work.execution"),
            WorkProjectionSequenceV1::new(4),
            WorkVersion::initial(),
            id::<ProposalId>("proposal.work.execution"),
        )
        .unwrap();
        let requested_route = route("route.requested");
        WorkAttemptV1::new(
            identity.clone(),
            projection_binding.clone(),
            execution_envelope(identity, projection_binding, requested_route.clone()),
            lease(1),
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            requested_route,
            None,
            None,
        )
        .unwrap()
    }

    #[derive(Clone)]
    struct FakePersistence {
        attempt: Arc<Mutex<Option<WorkAttemptV1>>>,
        reject_cas: Arc<Mutex<bool>>,
    }

    impl FakePersistence {
        fn seeded(attempt: WorkAttemptV1) -> Self {
            Self {
                attempt: Arc::new(Mutex::new(Some(attempt))),
                reject_cas: Arc::new(Mutex::new(false)),
            }
        }
    }

    impl WorkAttemptPersistencePort for FakePersistence {
        fn load(
            &self,
            _authority: &WorkAuthority,
            identity: &WorkAttemptIdentityV1,
        ) -> Result<Option<WorkAttemptV1>, WorkExecutionPersistenceError> {
            Ok(self
                .attempt
                .lock()
                .unwrap()
                .as_ref()
                .filter(|attempt| attempt.identity() == identity)
                .cloned())
        }

        fn insert(
            &self,
            _authority: &WorkAuthority,
            attempt: &WorkAttemptV1,
        ) -> Result<(), WorkExecutionPersistenceError> {
            let mut stored = self.attempt.lock().unwrap();
            if stored.is_some() {
                return Err(WorkExecutionPersistenceError::Conflict);
            }
            *stored = Some(attempt.clone());
            Ok(())
        }

        fn compare_and_swap(
            &self,
            _authority: &WorkAuthority,
            expected: &WorkAttemptV1,
            replacement: &WorkAttemptV1,
        ) -> Result<(), WorkExecutionPersistenceError> {
            if *self.reject_cas.lock().unwrap() {
                return Err(WorkExecutionPersistenceError::Conflict);
            }
            let mut stored = self.attempt.lock().unwrap();
            if stored.as_ref() != Some(expected) {
                return Err(WorkExecutionPersistenceError::Conflict);
            }
            *stored = Some(replacement.clone());
            Ok(())
        }
    }

    #[test]
    fn lifecycle_persists_bounded_progress_artifacts_and_terminal_replay() {
        let attempt = leased_attempt("attempt.work.lifecycle");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt));

        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();
        service
            .publish_progress(
                &authority(),
                &identity,
                &lease(1),
                WorkAttemptProgressV1::new(1, 2).unwrap(),
            )
            .unwrap();
        let artifact = WorkArtifactRefV1::new(
            id::<WorkArtifactId>("artifact.work.lifecycle"),
            digest('a'),
            42,
        )
        .unwrap();
        service
            .publish_artifact(&authority(), &identity, &lease(1), artifact.clone())
            .unwrap();

        let terminal = WorkTerminalEvidenceV1::succeeded(digest('b'), UtcMicros(20)).unwrap();
        let completed = service
            .terminalize(&authority(), &identity, &lease(1), terminal.clone())
            .unwrap();
        let replayed = service
            .terminalize(&authority(), &identity, &lease(1), terminal)
            .unwrap();
        assert_eq!(completed, replayed);
        assert_eq!(completed.progress().unwrap().completed(), 1);
        assert_eq!(completed.artifacts(), &[artifact]);
        assert!(completed.is_terminal());
    }

    fn cancellation_request(request_id: &str, requested_at: i64) -> WorkCancellationRequestV1 {
        WorkCancellationRequestV1::new(
            id::<WorkCancellationRequestId>(request_id),
            UtcMicros(requested_at),
        )
        .unwrap()
    }

    /// Retrying a refused provider stop replays the same request, so the same
    /// request must be idempotent while a different one must not silently
    /// rewrite who asked for the cancellation.
    #[test]
    fn replaying_a_cancellation_keeps_the_original_request_and_refuses_another() {
        let attempt = leased_attempt("attempt.work.cancel.replay");
        let identity = attempt.identity().clone();
        let persistence = FakePersistence::seeded(attempt);
        let service = WorkExecutionService::new(persistence.clone());
        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();
        let original = cancellation_request("cancel.work.first", 30);

        let requested = service
            .request_cancellation(&authority(), &identity, &lease(1), original.clone())
            .unwrap();
        let replayed = service
            .request_cancellation(&authority(), &identity, &lease(1), original.clone())
            .unwrap();
        assert_eq!(requested, replayed);

        assert_eq!(
            service
                .request_cancellation(
                    &authority(),
                    &identity,
                    &lease(1),
                    cancellation_request("cancel.work.second", 31)
                )
                .unwrap_err(),
            WorkExecutionError::TerminalConflict
        );
        assert_eq!(
            persistence
                .load(&authority(), &identity)
                .unwrap()
                .unwrap()
                .cancellation(),
            &WorkCancellationStateV1::Requested(original),
            "a refused second request must leave the original requester recorded"
        );
    }

    /// A settled attempt has no execution left to hold, so raising its fence
    /// would only let a caller keep asserting ownership of a closed attempt.
    #[test]
    fn a_terminal_attempt_refuses_a_lease_renewal() {
        let attempt = leased_attempt("attempt.work.renew.terminal");
        let identity = attempt.identity().clone();
        let persistence = FakePersistence::seeded(attempt);
        let service = WorkExecutionService::new(persistence.clone());
        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();
        service
            .terminalize(
                &authority(),
                &identity,
                &lease(1),
                WorkTerminalEvidenceV1::succeeded(digest('b'), UtcMicros(20)).unwrap(),
            )
            .unwrap();

        assert_eq!(
            service
                .renew_lease(&authority(), &identity, &lease(1), lease(2))
                .unwrap_err(),
            WorkExecutionError::TerminalConflict
        );
        assert_eq!(
            persistence
                .load(&authority(), &identity)
                .unwrap()
                .unwrap()
                .lease(),
            &lease(1),
            "a refused renewal must not raise the recorded fence"
        );
    }

    #[test]
    fn stale_compare_and_swap_is_never_reported_as_success() {
        let attempt = leased_attempt("attempt.work.cas");
        let identity = attempt.identity().clone();
        let persistence = FakePersistence::seeded(attempt);
        *persistence.reject_cas.lock().unwrap() = true;
        let service = WorkExecutionService::new(persistence.clone());

        assert_eq!(
            service
                .start(
                    &authority(),
                    &identity,
                    &lease(1),
                    WorkRecoveryStateV1::Fresh,
                    route("route.actual"),
                )
                .unwrap_err(),
            WorkExecutionError::Persistence(WorkExecutionPersistenceError::Conflict)
        );
        assert_eq!(
            persistence
                .load(&authority(), &identity)
                .unwrap()
                .unwrap()
                .state(),
            WorkAttemptStateV1::Leased,
            "a refused compare-and-swap must leave no running intent behind"
        );
    }

    #[test]
    fn replayed_start_re_records_the_same_running_intent() {
        let attempt = leased_attempt("attempt.work.replay");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt));

        let first = service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();
        let replayed = service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();

        assert_eq!(first, replayed);
        assert_eq!(replayed.state(), WorkAttemptStateV1::Running);
    }

    #[test]
    fn recovery_required_carries_the_predecessor_and_never_names_itself() {
        let attempt = leased_attempt("attempt.work.recovery");
        let identity = attempt.identity().clone();
        let predecessor = id::<AttemptId>("attempt.work.predecessor");
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt));
        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Restarted {
                    source_attempt_id: predecessor.clone(),
                    reason: WorkRestartReasonV1::ProcessLost,
                },
                route("route.actual"),
            )
            .unwrap();

        let recovering = service
            .require_recovery(
                &authority(),
                &identity,
                &lease(1),
                WorkRestartReasonV1::ProviderUnavailable,
            )
            .unwrap();

        assert_eq!(recovering.state(), WorkAttemptStateV1::RecoveryRequired);
        assert_eq!(
            recovering.recovery(),
            &WorkRecoveryStateV1::RecoveryRequired {
                source_attempt_id: Some(predecessor),
                reason: WorkRestartReasonV1::ProviderUnavailable,
            }
        );
    }

    #[test]
    fn a_first_attempt_lost_before_it_resumed_anything_can_still_require_recovery() {
        let attempt = leased_attempt("attempt.work.fresh-recovery");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt));
        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
                route("route.actual"),
            )
            .unwrap();

        let recovering = service
            .require_recovery(
                &authority(),
                &identity,
                &lease(1),
                WorkRestartReasonV1::ProcessLost,
            )
            .unwrap();

        assert_eq!(recovering.state(), WorkAttemptStateV1::RecoveryRequired);
        assert_eq!(
            recovering.recovery(),
            &WorkRecoveryStateV1::RecoveryRequired {
                source_attempt_id: None,
                reason: WorkRestartReasonV1::ProcessLost,
            },
            "a first attempt has no predecessor and must not name itself"
        );
        assert_ne!(
            recovering.recovery().source_attempt_id(),
            Some(identity.attempt_id())
        );
    }

    #[test]
    fn resumed_attempt_binds_recovery_to_a_different_attempt() {
        let attempt = leased_attempt("attempt.work.resumed");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt));
        let recovery = WorkRecoveryStateV1::Resumed {
            source_attempt_id: id::<AttemptId>("attempt.work.original"),
            checkpoint: None,
        };

        let running = service
            .start(
                &authority(),
                &identity,
                &lease(1),
                recovery.clone(),
                route("route.actual"),
            )
            .unwrap();
        assert_eq!(running.recovery(), &recovery);
        assert_eq!(running.state(), WorkAttemptStateV1::Running);
    }
}
