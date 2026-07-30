use std::fmt::Display;

use tracedecay_domain::{
    WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAttemptProgressV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationAcknowledgementV1, WorkCancellationEscalationV1, WorkCancellationRequestV1,
    WorkCancellationStateV1, WorkLeaseFenceV1, WorkProjectionSnapshotV1, WorkProviderRouteV1,
    WorkRecoveryStateV1, WorkRestartReasonV1, WorkRuntimeContractError, WorkTerminalEvidenceV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkExecutionPersistenceError {
    Conflict,
    Unavailable(String),
}

impl Display for WorkExecutionPersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => formatter.write_str("work attempt changed concurrently"),
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

pub trait WorkProviderExecutionPort: Send + Sync {
    fn start(
        &self,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkProviderRouteV1, WorkProviderExecutionError>;

    fn request_cancellation(
        &self,
        attempt: &WorkAttemptV1,
        request: &WorkCancellationRequestV1,
    ) -> Result<(), WorkProviderExecutionError>;
}

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

pub struct WorkExecutionService<S, P> {
    persistence: S,
    provider: P,
}

impl<S, P> WorkExecutionService<S, P>
where
    S: WorkAttemptPersistencePort,
    P: WorkProviderExecutionPort,
{
    pub const fn new(persistence: S, provider: P) -> Self {
        Self {
            persistence,
            provider,
        }
    }

    pub fn acquire_lease(
        &self,
        authority: &WorkAuthority,
        snapshot: &WorkProjectionSnapshotV1,
        identity: WorkAttemptIdentityV1,
        projection_binding: WorkAttemptProjectionBindingV1,
        lease: WorkLeaseFenceV1,
        requested_route: WorkProviderRouteV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt = WorkAttemptV1::new(
            identity,
            projection_binding,
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
        attempt.validate_snapshot(snapshot)?;
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
        if replacement.lease_id() != expected.lease_id() || replacement.epoch() <= expected.epoch()
        {
            return Err(WorkExecutionError::StaleLease);
        }
        let next = rebuild_attempt(&current, replacement)?;
        self.persistence
            .compare_and_swap(authority, &current, &next)?;
        Ok(next)
    }

    pub fn start(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        recovery: WorkRecoveryStateV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        let actual_route = self.provider.start(&current)?;
        self.transition(
            authority,
            current,
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            recovery,
            Some(actual_route),
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

    pub fn request_cancellation(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        request: WorkCancellationRequestV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let current = self.load_with_fence(authority, identity, lease)?;
        self.provider.request_cancellation(&current, &request)?;
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
            source_attempt_id: current.identity().attempt_id().clone(),
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
    use std::sync::{Arc, Mutex};

    use tracedecay_domain::{
        ActorId, AttemptId, ManifestDigest, ProjectId, ProjectionGenerationId, ProviderId,
        RepositoryId, RunId, TaskId, UtcMicros, WorkArtifactId, WorkFenceEpochV1, WorkLeaseId,
        WorkProjectionSequenceV1, WorkProviderRouteId, WorkVersion, WorktreeId,
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
            id::<ProviderId>("provider.work.execution"),
            id::<WorkProviderRouteId>(value),
        )
        .unwrap()
    }

    fn leased_attempt(attempt_id: &str) -> WorkAttemptV1 {
        WorkAttemptV1::new(
            identity(attempt_id),
            WorkAttemptProjectionBindingV1::new(
                id::<ProjectionGenerationId>("generation.work.execution"),
                WorkProjectionSequenceV1::new(4),
                WorkVersion::initial(),
            )
            .unwrap(),
            lease(1),
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            route("route.requested"),
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

    #[derive(Clone, Copy)]
    struct FakeProvider;

    impl WorkProviderExecutionPort for FakeProvider {
        fn start(
            &self,
            _attempt: &WorkAttemptV1,
        ) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
            Ok(route("route.actual"))
        }

        fn request_cancellation(
            &self,
            _attempt: &WorkAttemptV1,
            _request: &WorkCancellationRequestV1,
        ) -> Result<(), WorkProviderExecutionError> {
            Ok(())
        }
    }

    #[test]
    fn lifecycle_persists_bounded_progress_artifacts_and_terminal_replay() {
        let attempt = leased_attempt("attempt.work.lifecycle");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt), FakeProvider);

        service
            .start(
                &authority(),
                &identity,
                &lease(1),
                WorkRecoveryStateV1::Fresh,
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

    #[test]
    fn stale_compare_and_swap_is_never_reported_as_success() {
        let attempt = leased_attempt("attempt.work.cas");
        let identity = attempt.identity().clone();
        let persistence = FakePersistence::seeded(attempt);
        *persistence.reject_cas.lock().unwrap() = true;
        let service = WorkExecutionService::new(persistence, FakeProvider);

        assert_eq!(
            service
                .start(
                    &authority(),
                    &identity,
                    &lease(1),
                    WorkRecoveryStateV1::Fresh,
                )
                .unwrap_err(),
            WorkExecutionError::Persistence(WorkExecutionPersistenceError::Conflict)
        );
    }

    #[test]
    fn resumed_attempt_binds_recovery_to_a_different_attempt() {
        let attempt = leased_attempt("attempt.work.resumed");
        let identity = attempt.identity().clone();
        let service = WorkExecutionService::new(FakePersistence::seeded(attempt), FakeProvider);
        let recovery = WorkRecoveryStateV1::Resumed {
            source_attempt_id: id::<AttemptId>("attempt.work.original"),
            checkpoint: None,
        };

        let running = service
            .start(&authority(), &identity, &lease(1), recovery.clone())
            .unwrap();
        assert_eq!(running.recovery(), &recovery);
        assert_eq!(running.state(), WorkAttemptStateV1::Running);
    }
}
