//! Workflow definition activation, deterministic placement, and task handoff contracts.
//!
//! These services are transport- and storage-neutral. Production composition
//! supplies the canonical Work and automation authorities through the ports
//! defined here; this module does not create a second scheduler or Work store.

use std::fmt::{self, Display};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkProviderRouteV1, WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowStepId, WorktreeId,
    canonical_sha256,
};

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
    pub definition_version: u64,
    pub run_id: RunId,
    pub step_id: WorkflowStepId,
    pub task_id: TaskId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPlacementCandidateV1 {
    pub route: WorkProviderRouteV1,
    pub priority: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowPlacementError {
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
        let mut candidates = self.placement.candidates(request)?;
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
        if !(32..=512).contains(&secret.len())
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffScopeV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub from_actor_id: ActorId,
    pub to_actor_id: ActorId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffGrantV1 {
    pub scope: TaskHandoffScopeV1,
    token_digest: ManifestDigest,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
}

impl TaskHandoffGrantV1 {
    pub fn token_digest(&self) -> &ManifestDigest {
        &self.token_digest
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
    Unauthorized,
    InvalidExpiry,
    Conflict,
    Missing,
    ScopeMismatch,
    Expired,
    Replay,
    AuthorityUnavailable(String),
}

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
        if issuer != &scope.from_actor_id {
            return Err(TaskHandoffError::Unauthorized);
        }
        if expires_at <= issued_at {
            return Err(TaskHandoffError::InvalidExpiry);
        }
        let grant = TaskHandoffGrantV1 {
            scope,
            token_digest: token.digest()?,
            issued_at,
            expires_at,
        };
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
        if redeemer != &expected_scope.to_actor_id {
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
