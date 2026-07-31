//! Workflow definition activation, deterministic placement, and task handoff contracts.
//!
//! These services are transport- and storage-neutral. Production composition
//! supplies the canonical Work and automation authorities through the ports
//! defined here; this module does not create a second scheduler or Work store.

use std::fmt::{self, Display};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros,
    WorkProviderRouteV1, WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowStepId, WorktreeId,
    canonical_sha256,
};

/// Upper inclusive bound for calibrated placement scores (micros of unit interval).
pub const MAX_CALIBRATED_SCORE_MICROS: u32 = 1_000_000;

/// Maximum task-handoff grant lifetime (60 seconds), as `UtcMicros` duration micros.
pub const MAX_TASK_HANDOFF_LIFETIME_MICROS: UtcMicros = UtcMicros(60_000_000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowDefinitionAuthorityError {
    AlreadyExists,
    Conflict,
    Unavailable(String),
}

pub trait WorkflowDefinitionAuthorityPort: Send + Sync {
    fn insert(
        &self,
        definition: &WorkflowDefinitionV1,
    ) -> Result<(), WorkflowDefinitionAuthorityError>;

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionV1>, WorkflowDefinitionAuthorityError>;

    fn active_version(
        &self,
        definition_id: &WorkflowDefinitionId,
    ) -> Result<Option<u64>, WorkflowDefinitionAuthorityError>;

    fn compare_and_swap_activation(
        &self,
        definition_id: &WorkflowDefinitionId,
        expected_version: Option<u64>,
        replacement_version: u64,
    ) -> Result<(), WorkflowDefinitionAuthorityError>;
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowActivationV1 {
    pub definition_id: WorkflowDefinitionId,
    pub active_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowCoordinationError {
    InvalidDefinition,
    ImmutableDefinitionConflict,
    DefinitionNotFound,
    StaleActivation,
    AuthorityUnavailable(String),
}

impl Display for WorkflowCoordinationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDefinition => formatter.write_str("workflow definition is invalid"),
            Self::ImmutableDefinitionConflict => {
                formatter.write_str("workflow definition identity and version are immutable")
            }
            Self::DefinitionNotFound => formatter.write_str("workflow definition was not found"),
            Self::StaleActivation => {
                formatter.write_str("workflow activation changed concurrently")
            }
            Self::AuthorityUnavailable(message) => {
                write!(
                    formatter,
                    "workflow definition authority unavailable: {message}"
                )
            }
        }
    }
}

impl std::error::Error for WorkflowCoordinationError {}

pub struct WorkflowDefinitionService<P> {
    authority: P,
}

impl<P> WorkflowDefinitionService<P>
where
    P: WorkflowDefinitionAuthorityPort,
{
    pub const fn new(authority: P) -> Self {
        Self { authority }
    }

    pub fn register(
        &self,
        definition: WorkflowDefinitionV1,
    ) -> Result<WorkflowDefinitionV1, WorkflowCoordinationError> {
        definition
            .validate()
            .map_err(|_| WorkflowCoordinationError::InvalidDefinition)?;
        match self.authority.insert(&definition) {
            Ok(()) => Ok(definition),
            Err(WorkflowDefinitionAuthorityError::AlreadyExists) => {
                let existing = self
                    .authority
                    .load(definition.definition_id(), definition.definition_version())
                    .map_err(coordination_authority_error)?
                    .ok_or(WorkflowCoordinationError::ImmutableDefinitionConflict)?;
                if existing == definition {
                    Ok(existing)
                } else {
                    Err(WorkflowCoordinationError::ImmutableDefinitionConflict)
                }
            }
            Err(error) => Err(coordination_authority_error(error)),
        }
    }

    pub fn activate(
        &self,
        definition_id: &WorkflowDefinitionId,
        expected_active_version: Option<u64>,
        replacement_version: u64,
    ) -> Result<WorkflowActivationV1, WorkflowCoordinationError> {
        if replacement_version == 0 {
            return Err(WorkflowCoordinationError::InvalidDefinition);
        }
        self.authority
            .load(definition_id, replacement_version)
            .map_err(coordination_authority_error)?
            .ok_or(WorkflowCoordinationError::DefinitionNotFound)?;

        let current = self
            .authority
            .active_version(definition_id)
            .map_err(coordination_authority_error)?;
        if current != expected_active_version {
            return Err(WorkflowCoordinationError::StaleActivation);
        }
        self.authority
            .compare_and_swap_activation(
                definition_id,
                expected_active_version,
                replacement_version,
            )
            .map_err(coordination_authority_error)?;
        Ok(WorkflowActivationV1 {
            definition_id: definition_id.clone(),
            active_version: replacement_version,
        })
    }
}

fn coordination_authority_error(
    error: WorkflowDefinitionAuthorityError,
) -> WorkflowCoordinationError {
    match error {
        WorkflowDefinitionAuthorityError::AlreadyExists => {
            WorkflowCoordinationError::ImmutableDefinitionConflict
        }
        WorkflowDefinitionAuthorityError::Conflict => WorkflowCoordinationError::StaleActivation,
        WorkflowDefinitionAuthorityError::Unavailable(message) => {
            WorkflowCoordinationError::AuthorityUnavailable(message)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlacementRequestV1 {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub task_id: TaskId,
    pub required_expertise_digest: ManifestDigest,
    pub calibration_profile_digest: ManifestDigest,
    #[schemars(range(min = 0, max = 1_000_000))]
    pub minimum_calibrated_score_micros: u32,
}

impl WorkflowPlacementRequestV1 {
    pub fn validate(&self) -> Result<(), WorkflowPlacementError> {
        if self.definition_version == 0
            || self.minimum_calibrated_score_micros > MAX_CALIBRATED_SCORE_MICROS
        {
            return Err(WorkflowPlacementError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlacementCandidateV1 {
    pub route: WorkProviderRouteV1,
    pub priority: u32,
    pub expertise_digest: ManifestDigest,
    pub calibration_profile_digest: ManifestDigest,
    #[schemars(range(min = 0, max = 1_000_000))]
    pub calibrated_score_micros: u32,
}

impl WorkflowPlacementCandidateV1 {
    fn evidence_matches(&self, request: &WorkflowPlacementRequestV1) -> bool {
        self.expertise_digest == request.required_expertise_digest
            && self.calibration_profile_digest == request.calibration_profile_digest
            && self.calibrated_score_micros <= MAX_CALIBRATED_SCORE_MICROS
            && self.calibrated_score_micros >= request.minimum_calibrated_score_micros
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowPlacementError {
    InvalidRequest,
    Unavailable { step_id: WorkflowStepId },
    AuthorityUnavailable(String),
}

pub trait WorkflowPlacementPort: Send + Sync {
    fn candidates(
        &self,
        request: &WorkflowPlacementRequestV1,
    ) -> Result<Vec<WorkflowPlacementCandidateV1>, WorkflowPlacementError>;
}

pub struct WorkflowPlacementService<P> {
    placement: P,
}

impl<P> WorkflowPlacementService<P>
where
    P: WorkflowPlacementPort,
{
    pub const fn new(placement: P) -> Self {
        Self { placement }
    }

    pub fn place(
        &self,
        request: &WorkflowPlacementRequestV1,
    ) -> Result<WorkProviderRouteV1, WorkflowPlacementError> {
        request.validate()?;
        let mut candidates = self.placement.candidates(request)?;
        candidates.retain(|candidate| candidate.evidence_matches(request));
        candidates.sort_by(|left, right| {
            (
                left.priority,
                left.route.provider_id().as_str(),
                left.route.route_id().as_str(),
            )
                .cmp(&(
                    right.priority,
                    right.route.provider_id().as_str(),
                    right.route.route_id().as_str(),
                ))
        });
        candidates
            .into_iter()
            .next()
            .map(|candidate| candidate.route)
            .ok_or_else(|| WorkflowPlacementError::Unavailable {
                step_id: request.step_id.clone(),
            })
    }
}

pub struct TaskHandoffToken {
    secret: String,
}

impl TaskHandoffToken {
    pub fn new(secret: String) -> Result<Self, TaskHandoffError> {
        let byte_len = secret.len();
        if !(32..=512).contains(&byte_len)
            || secret.trim() != secret
            || secret.chars().any(char::is_control)
        {
            return Err(TaskHandoffError::InvalidToken);
        }
        Ok(Self { secret })
    }

    fn digest(&self) -> Result<ManifestDigest, TaskHandoffError> {
        canonical_sha256(&("tracedecay.application.task-handoff.v1", &self.secret))
            .map_err(|_| TaskHandoffError::InvalidToken)
    }
}

impl fmt::Debug for TaskHandoffToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TaskHandoffToken([REDACTED])")
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffScopeV1 {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    definition_version: u64,
    step_id: WorkflowStepId,
    task_id: TaskId,
    thread_id: ThreadId,
    run_id: RunId,
    from_actor_id: ActorId,
    to_actor_id: ActorId,
}

impl TaskHandoffScopeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        definition_id: WorkflowDefinitionId,
        definition_version: u64,
        step_id: WorkflowStepId,
        task_id: TaskId,
        thread_id: ThreadId,
        run_id: RunId,
        from_actor_id: ActorId,
        to_actor_id: ActorId,
    ) -> Result<Self, TaskHandoffError> {
        let scope = Self {
            project_id,
            repository_id,
            worktree_id,
            definition_id,
            definition_version,
            step_id,
            task_id,
            thread_id,
            run_id,
            from_actor_id,
            to_actor_id,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<(), TaskHandoffError> {
        if self.definition_version == 0 {
            return Err(TaskHandoffError::InvalidScope);
        }
        Ok(())
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

    pub fn definition_id(&self) -> &WorkflowDefinitionId {
        &self.definition_id
    }

    pub fn definition_version(&self) -> u64 {
        self.definition_version
    }

    pub fn step_id(&self) -> &WorkflowStepId {
        &self.step_id
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn thread_id(&self) -> &ThreadId {
        &self.thread_id
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn from_actor_id(&self) -> &ActorId {
        &self.from_actor_id
    }

    pub fn to_actor_id(&self) -> &ActorId {
        &self.to_actor_id
    }
}

impl<'de> Deserialize<'de> for TaskHandoffScopeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            project_id: ProjectId,
            repository_id: RepositoryId,
            worktree_id: WorktreeId,
            definition_id: WorkflowDefinitionId,
            definition_version: u64,
            step_id: WorkflowStepId,
            task_id: TaskId,
            thread_id: ThreadId,
            run_id: RunId,
            from_actor_id: ActorId,
            to_actor_id: ActorId,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.project_id,
            wire.repository_id,
            wire.worktree_id,
            wire.definition_id,
            wire.definition_version,
            wire.step_id,
            wire.task_id,
            wire.thread_id,
            wire.run_id,
            wire.from_actor_id,
            wire.to_actor_id,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffGrantV1 {
    scope: TaskHandoffScopeV1,
    token_digest: ManifestDigest,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
}

impl TaskHandoffGrantV1 {
    pub fn new(
        scope: TaskHandoffScopeV1,
        token_digest: ManifestDigest,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, TaskHandoffError> {
        let grant = Self {
            scope,
            token_digest,
            issued_at,
            expires_at,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<(), TaskHandoffError> {
        self.scope.validate()?;
        if !(self.issued_at < self.expires_at) {
            return Err(TaskHandoffError::InvalidExpiry);
        }
        let Some(lifetime_micros) = self.expires_at.0.checked_sub(self.issued_at.0) else {
            return Err(TaskHandoffError::InvalidExpiry);
        };
        if lifetime_micros > MAX_TASK_HANDOFF_LIFETIME_MICROS.0 {
            return Err(TaskHandoffError::InvalidExpiry);
        }
        Ok(())
    }

    pub fn scope(&self) -> &TaskHandoffScopeV1 {
        &self.scope
    }

    pub fn token_digest(&self) -> &ManifestDigest {
        &self.token_digest
    }

    pub fn issued_at(&self) -> &UtcMicros {
        &self.issued_at
    }

    pub fn expires_at(&self) -> &UtcMicros {
        &self.expires_at
    }
}

impl<'de> Deserialize<'de> for TaskHandoffGrantV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            scope: TaskHandoffScopeV1,
            token_digest: ManifestDigest,
            issued_at: UtcMicros,
            expires_at: UtcMicros,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.scope,
            wire.token_digest,
            wire.issued_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskHandoffConsumeOutcome {
    Consumed,
    Missing,
    ScopeMismatch,
    Expired,
    Replay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskHandoffAuthorityError {
    Conflict,
    Unavailable(String),
}

pub trait TaskHandoffAuthorityPort: Send + Sync {
    fn issue(&self, grant: &TaskHandoffGrantV1) -> Result<(), TaskHandoffAuthorityError>;

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected_scope: &TaskHandoffScopeV1,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskHandoffError {
    InvalidToken,
    InvalidScope,
    Unauthorized,
    InvalidExpiry,
    Conflict,
    Missing,
    ScopeMismatch,
    Expired,
    Replay,
    AuthorityUnavailable(String),
}

impl Display for TaskHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => formatter.write_str("task handoff token is invalid"),
            Self::InvalidScope => formatter.write_str("task handoff scope is invalid"),
            Self::Unauthorized => formatter.write_str("task handoff actor is unauthorized"),
            Self::InvalidExpiry => formatter.write_str("task handoff expiry is invalid"),
            Self::Conflict => formatter.write_str("task handoff grant conflicts"),
            Self::Missing => formatter.write_str("task handoff grant is missing"),
            Self::ScopeMismatch => formatter.write_str("task handoff scope mismatch"),
            Self::Expired => formatter.write_str("task handoff grant expired"),
            Self::Replay => formatter.write_str("task handoff grant already consumed"),
            Self::AuthorityUnavailable(message) => {
                write!(formatter, "task handoff authority unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for TaskHandoffError {}

pub struct TaskHandoffService<P> {
    authority: P,
}

impl<P> TaskHandoffService<P>
where
    P: TaskHandoffAuthorityPort,
{
    pub const fn new(authority: P) -> Self {
        Self { authority }
    }

    pub fn issue(
        &self,
        issuer: &ActorId,
        scope: TaskHandoffScopeV1,
        token: &TaskHandoffToken,
        expires_at: UtcMicros,
        issued_at: UtcMicros,
    ) -> Result<TaskHandoffGrantV1, TaskHandoffError> {
        if issuer != scope.from_actor_id() {
            return Err(TaskHandoffError::Unauthorized);
        }
        let grant = TaskHandoffGrantV1::new(scope, token.digest()?, issued_at, expires_at)?;
        self.authority
            .issue(&grant)
            .map_err(handoff_authority_error)?;
        Ok(grant)
    }

    pub fn redeem(
        &self,
        token: &TaskHandoffToken,
        expected_scope: &TaskHandoffScopeV1,
        redeemer: &ActorId,
        consumed_at: UtcMicros,
    ) -> Result<(), TaskHandoffError> {
        expected_scope.validate()?;
        if redeemer != expected_scope.to_actor_id() {
            return Err(TaskHandoffError::Unauthorized);
        }
        match self
            .authority
            .consume(&token.digest()?, expected_scope, consumed_at)
            .map_err(handoff_authority_error)?
        {
            TaskHandoffConsumeOutcome::Consumed => Ok(()),
            TaskHandoffConsumeOutcome::Missing => Err(TaskHandoffError::Missing),
            TaskHandoffConsumeOutcome::ScopeMismatch => Err(TaskHandoffError::ScopeMismatch),
            TaskHandoffConsumeOutcome::Expired => Err(TaskHandoffError::Expired),
            TaskHandoffConsumeOutcome::Replay => Err(TaskHandoffError::Replay),
        }
    }
}

fn handoff_authority_error(error: TaskHandoffAuthorityError) -> TaskHandoffError {
    match error {
        TaskHandoffAuthorityError::Conflict => TaskHandoffError::Conflict,
        TaskHandoffAuthorityError::Unavailable(message) => {
            TaskHandoffError::AuthorityUnavailable(message)
        }
    }
}
