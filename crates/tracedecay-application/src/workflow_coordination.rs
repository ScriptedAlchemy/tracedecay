//! Workflow definition storage and task handoff contracts.
//!
//! These services are transport- and storage-neutral. Production composition
//! supplies the canonical Work and automation authorities through the ports
//! defined here; this module does not create a second scheduler or Work store.

use std::collections::BTreeSet;
use std::fmt::{self, Display};

use crate::RequestContext;
use crate::work_handoff_frontier::WorkHandoffFrontierV1;
use crate::workflow_admission::{
    WorkflowCatalogAdmissionError, admit_workflow_definition_operations,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros,
    WorkflowDefinition, WorkflowDefinitionId, WorkflowStepId, WorktreeId, canonical_sha256,
};

/// Fixed task-handoff grant lifetime (60 seconds), as `UtcMicros` duration micros.
pub const TASK_HANDOFF_LIFETIME_MICROS: UtcMicros = UtcMicros(60_000_000);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowDefinitionAuthorityError {
    AlreadyExists,
    Conflict,
    Unavailable(String),
}

/// Durable lifecycle disposition of one immutable workflow definition version.
///
/// Plan 32 ("Typed workflow definitions"): "Lifecycle retains candidate,
/// validate, activate, retire, reject, list, get, diff, and history operations
/// through the same application surfaces." The definition payload itself stays
/// immutable — "Editing creates a new version; admitted runs remain pinned" —
/// so the disposition is a separate revisioned aggregate keyed by the same
/// definition identity.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDefinitionLifecycleState {
    Candidate,
    Validated,
    Active,
    Retired,
    Rejected,
}

impl WorkflowDefinitionLifecycleState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Validated => "validated",
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Rejected => "rejected",
        }
    }

    pub fn from_state_key(key: &str) -> Option<Self> {
        match key {
            "candidate" => Some(Self::Candidate),
            "validated" => Some(Self::Validated),
            "active" => Some(Self::Active),
            "retired" => Some(Self::Retired),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    /// Retire and reject are terminal dispositions: nothing transitions out.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired | Self::Rejected)
    }
}

/// The three lifecycle transitions that mutate a stored disposition.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLifecycleOperation {
    Activate,
    Retire,
    Reject,
}

const ACTIVATE_FROM_CANDIDATE: &[WorkflowDefinitionLifecycleState] = &[
    WorkflowDefinitionLifecycleState::Validated,
    WorkflowDefinitionLifecycleState::Active,
];
const ACTIVATE_FROM_VALIDATED: &[WorkflowDefinitionLifecycleState] =
    &[WorkflowDefinitionLifecycleState::Active];
const RETIRE_FROM_ACTIVE: &[WorkflowDefinitionLifecycleState] =
    &[WorkflowDefinitionLifecycleState::Retired];
const REJECT_FROM_OPEN: &[WorkflowDefinitionLifecycleState] =
    &[WorkflowDefinitionLifecycleState::Rejected];

impl WorkflowLifecycleOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Retire => "retire",
            Self::Reject => "reject",
        }
    }

    pub fn from_operation_key(key: &str) -> Option<Self> {
        match key {
            "activate" => Some(Self::Activate),
            "retire" => Some(Self::Retire),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }

    /// Canonical edge table shared by every authority implementation.
    ///
    /// Plan 32: the retained lifecycle is `candidate -> validated -> active`
    /// with retire and reject as terminal dispositions, and "Unknown
    /// operations, cycles, dangling references, incompatible schemas,
    /// unbounded fan-out, privilege expansion, unsupported effects, or
    /// recursive generic execution reject before activation" — so activating a
    /// candidate records the intermediate `validated` disposition it had to
    /// clear, and every state it passes through gets its own immutable history
    /// entry. `None` names an illegal transition.
    pub const fn path_from(
        self,
        current: WorkflowDefinitionLifecycleState,
    ) -> Option<&'static [WorkflowDefinitionLifecycleState]> {
        match (self, current) {
            (Self::Activate, WorkflowDefinitionLifecycleState::Candidate) => {
                Some(ACTIVATE_FROM_CANDIDATE)
            }
            (Self::Activate, WorkflowDefinitionLifecycleState::Validated) => {
                Some(ACTIVATE_FROM_VALIDATED)
            }
            (Self::Retire, WorkflowDefinitionLifecycleState::Active) => Some(RETIRE_FROM_ACTIVE),
            (
                Self::Reject,
                WorkflowDefinitionLifecycleState::Candidate
                | WorkflowDefinitionLifecycleState::Validated,
            ) => Some(REJECT_FROM_OPEN),
            _ => None,
        }
    }
}

/// Revisioned lifecycle disposition of one definition version.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionDisposition {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    pub state: WorkflowDefinitionLifecycleState,
    #[schemars(range(min = 1))]
    pub revision: u64,
    pub transitioned_at: UtcMicros,
}

/// One immutable history entry appended by a lifecycle transition.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionTransitionEntry {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    pub operation: WorkflowLifecycleOperation,
    pub from_state: WorkflowDefinitionLifecycleState,
    pub to_state: WorkflowDefinitionLifecycleState,
    #[schemars(range(min = 1))]
    pub from_revision: u64,
    #[schemars(range(min = 2))]
    pub to_revision: u64,
    pub transitioned_at: UtcMicros,
}

/// Compare-and-swap command carried across the effect journal and applied by
/// the durable authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionLifecycleCommand {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    pub operation: WorkflowLifecycleOperation,
    #[schemars(range(min = 1))]
    pub expected_revision: u64,
    pub transitioned_at: UtcMicros,
}

/// Outcome of one attempted lifecycle transition on the durable authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowDefinitionTransitionOutcome {
    /// The transition ran and appended new immutable history entries.
    Applied(WorkflowDefinitionDisposition),
    /// The exact command already ran; the stored disposition is returned
    /// unchanged so replay stays observably identical.
    Replayed(WorkflowDefinitionDisposition),
    /// `expected_revision` did not name the stored revision.
    RevisionConflict(WorkflowDefinitionDisposition),
    /// The stored state has no edge for this operation.
    IllegalTransition(WorkflowDefinitionDisposition),
    /// No disposition exists for the named definition version.
    Missing,
}

pub trait WorkflowDefinitionAuthorityPort: Send + Sync {
    fn insert(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowDefinitionAuthorityError>;

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinition>, WorkflowDefinitionAuthorityError>;

    fn list(
        &self,
        definition_id: Option<&WorkflowDefinitionId>,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowDefinitionAuthorityError>;

    fn load_disposition(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionDisposition>, WorkflowDefinitionAuthorityError>;

    fn transition(
        &self,
        command: &WorkflowDefinitionLifecycleCommand,
    ) -> Result<WorkflowDefinitionTransitionOutcome, WorkflowDefinitionAuthorityError>;

    fn transition_history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionTransitionEntry>, WorkflowDefinitionAuthorityError>;
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionValidation {
    pub definition: WorkflowDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionDiff {
    pub definition_id: WorkflowDefinitionId,
    pub from_version: u64,
    pub to_version: u64,
    pub changed_steps: BTreeSet<WorkflowStepId>,
    pub policy_changed: bool,
    pub configuration_changed: bool,
    pub catalog_changed: bool,
}

/// Wire request for [`WorkflowDefinitionService::register`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionRegisterRequest {
    pub definition: WorkflowDefinition,
}

/// Wire request for [`WorkflowDefinitionService::validate`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionValidateRequest {
    pub definition: WorkflowDefinition,
}

/// Wire request for [`WorkflowDefinitionService::get`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionGetRequest {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
}

/// Wire request for [`WorkflowDefinitionService::list`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionListRequest {}

/// Wire request for [`WorkflowDefinitionService::history`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionHistoryRequest {
    pub definition_id: WorkflowDefinitionId,
}

/// Wire request for [`WorkflowDefinitionService::diff`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionDiffRequest {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub from_version: u64,
    #[schemars(range(min = 1))]
    pub to_version: u64,
}

/// Wire request for [`WorkflowDefinitionService::activate`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionActivateRequest {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    #[schemars(range(min = 1))]
    pub expected_revision: u64,
}

/// Wire request for [`WorkflowDefinitionService::retire`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionRetireRequest {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    #[schemars(range(min = 1))]
    pub expected_revision: u64,
}

/// Wire request for [`WorkflowDefinitionService::reject`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinitionRejectRequest {
    pub definition_id: WorkflowDefinitionId,
    #[schemars(range(min = 1))]
    pub definition_version: u64,
    #[schemars(range(min = 1))]
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowCoordinationError {
    InvalidDefinition,
    CatalogAdmissionDenied(WorkflowCatalogAdmissionError),
    ScopeMismatch,
    ImmutableDefinitionConflict,
    DefinitionNotFound,
    IllegalLifecycleTransition,
    LifecycleRevisionConflict,
    AuthorityUnavailable(String),
}

impl Display for WorkflowCoordinationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDefinition => formatter.write_str("workflow definition is invalid"),
            Self::CatalogAdmissionDenied(denial) => {
                write!(formatter, "workflow catalog admission denied: {denial}")
            }
            Self::ScopeMismatch => {
                formatter.write_str("workflow definition is outside the admitted project")
            }
            Self::ImmutableDefinitionConflict => {
                formatter.write_str("workflow definition identity and version are immutable")
            }
            Self::DefinitionNotFound => formatter.write_str("workflow definition was not found"),
            Self::IllegalLifecycleTransition => {
                formatter.write_str("workflow definition lifecycle transition is not legal")
            }
            Self::LifecycleRevisionConflict => formatter
                .write_str("workflow definition lifecycle revision did not match the expectation"),
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
        context: &RequestContext,
        definition: WorkflowDefinition,
    ) -> Result<WorkflowDefinition, WorkflowCoordinationError> {
        let definition = prepare_workflow_definition_registration(context, definition)?;
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

    /// The preflight for activation: structural shape plus tool-catalog
    /// semantic admission of every step operation.
    pub fn validate(
        &self,
        definition: WorkflowDefinition,
    ) -> Result<WorkflowDefinitionValidation, WorkflowCoordinationError> {
        definition
            .validate()
            .map_err(|_| WorkflowCoordinationError::InvalidDefinition)?;
        admit_workflow_definition_operations(&definition)
            .map_err(WorkflowCoordinationError::CatalogAdmissionDenied)?;
        Ok(WorkflowDefinitionValidation { definition })
    }

    pub fn get(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<WorkflowDefinition, WorkflowCoordinationError> {
        if definition_version == 0 {
            return Err(WorkflowCoordinationError::InvalidDefinition);
        }
        self.authority
            .load(definition_id, definition_version)
            .map_err(coordination_authority_error)?
            .ok_or(WorkflowCoordinationError::DefinitionNotFound)
    }

    pub fn list(&self) -> Result<Vec<WorkflowDefinition>, WorkflowCoordinationError> {
        self.authority
            .list(None)
            .map_err(coordination_authority_error)
    }

    pub fn history(
        &self,
        definition_id: &WorkflowDefinitionId,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowCoordinationError> {
        self.authority
            .list(Some(definition_id))
            .map_err(coordination_authority_error)
    }

    /// Admission every activation must clear before its lifecycle transition
    /// is journaled: structural revalidation plus tool-catalog admission of
    /// every step operation. The one authority both activation paths — this
    /// service and the daemon's journaled effect — run.
    pub fn admit_activation(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<(), WorkflowCoordinationError> {
        let definition = self.get(definition_id, definition_version)?;
        definition
            .validate()
            .map_err(|_| WorkflowCoordinationError::InvalidDefinition)?;
        admit_workflow_definition_operations(&definition)
            .map_err(WorkflowCoordinationError::CatalogAdmissionDenied)
    }

    /// Advances a registered definition version to `active`.
    ///
    /// Plan 32: "Unknown operations, cycles, dangling references, incompatible
    /// schemas, unbounded fan-out, privilege expansion, unsupported effects,
    /// or recursive generic execution reject before activation." The stored
    /// payload is revalidated and catalog-admitted here, and the
    /// `candidate -> validated -> active` path is recorded as immutable
    /// history entries by the authority.
    pub fn activate(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_revision: u64,
        transitioned_at: UtcMicros,
    ) -> Result<WorkflowDefinitionDisposition, WorkflowCoordinationError> {
        self.admit_activation(definition_id, definition_version)?;
        self.apply_lifecycle(WorkflowDefinitionLifecycleCommand {
            definition_id: definition_id.clone(),
            definition_version,
            operation: WorkflowLifecycleOperation::Activate,
            expected_revision,
            transitioned_at,
        })
    }

    /// Retires an active definition version. Plan 32 keeps retire a terminal
    /// disposition: admitted runs stay pinned to the version they admitted,
    /// and nothing transitions back out.
    pub fn retire(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_revision: u64,
        transitioned_at: UtcMicros,
    ) -> Result<WorkflowDefinitionDisposition, WorkflowCoordinationError> {
        self.get(definition_id, definition_version)?;
        self.apply_lifecycle(WorkflowDefinitionLifecycleCommand {
            definition_id: definition_id.clone(),
            definition_version,
            operation: WorkflowLifecycleOperation::Retire,
            expected_revision,
            transitioned_at,
        })
    }

    /// Rejects a candidate or validated definition version. Plan 32 keeps
    /// reject a terminal disposition alongside retire.
    pub fn reject(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_revision: u64,
        transitioned_at: UtcMicros,
    ) -> Result<WorkflowDefinitionDisposition, WorkflowCoordinationError> {
        self.get(definition_id, definition_version)?;
        self.apply_lifecycle(WorkflowDefinitionLifecycleCommand {
            definition_id: definition_id.clone(),
            definition_version,
            operation: WorkflowLifecycleOperation::Reject,
            expected_revision,
            transitioned_at,
        })
    }

    pub fn disposition(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<WorkflowDefinitionDisposition, WorkflowCoordinationError> {
        if definition_version == 0 {
            return Err(WorkflowCoordinationError::InvalidDefinition);
        }
        self.authority
            .load_disposition(definition_id, definition_version)
            .map_err(coordination_authority_error)?
            .ok_or(WorkflowCoordinationError::DefinitionNotFound)
    }

    pub fn lifecycle_history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionTransitionEntry>, WorkflowCoordinationError> {
        if definition_version == 0 {
            return Err(WorkflowCoordinationError::InvalidDefinition);
        }
        self.authority
            .transition_history(definition_id, definition_version)
            .map_err(coordination_authority_error)
    }

    fn apply_lifecycle(
        &self,
        command: WorkflowDefinitionLifecycleCommand,
    ) -> Result<WorkflowDefinitionDisposition, WorkflowCoordinationError> {
        if command.expected_revision == 0 {
            return Err(WorkflowCoordinationError::InvalidDefinition);
        }
        match self
            .authority
            .transition(&command)
            .map_err(coordination_authority_error)?
        {
            WorkflowDefinitionTransitionOutcome::Applied(disposition)
            | WorkflowDefinitionTransitionOutcome::Replayed(disposition) => Ok(disposition),
            WorkflowDefinitionTransitionOutcome::RevisionConflict(_) => {
                Err(WorkflowCoordinationError::LifecycleRevisionConflict)
            }
            WorkflowDefinitionTransitionOutcome::IllegalTransition(_) => {
                Err(WorkflowCoordinationError::IllegalLifecycleTransition)
            }
            WorkflowDefinitionTransitionOutcome::Missing => {
                Err(WorkflowCoordinationError::DefinitionNotFound)
            }
        }
    }

    pub fn diff(
        &self,
        definition_id: &WorkflowDefinitionId,
        from_version: u64,
        to_version: u64,
    ) -> Result<WorkflowDefinitionDiff, WorkflowCoordinationError> {
        let from = self.get(definition_id, from_version)?;
        let to = self.get(definition_id, to_version)?;
        let step_ids = from
            .steps()
            .iter()
            .chain(to.steps())
            .map(|step| step.step_id.clone())
            .collect::<BTreeSet<_>>();
        let changed_steps = step_ids
            .into_iter()
            .filter(|step_id| {
                let from_step = from.steps().iter().find(|step| &step.step_id == step_id);
                let to_step = to.steps().iter().find(|step| &step.step_id == step_id);
                from_step != to_step
            })
            .collect();
        Ok(WorkflowDefinitionDiff {
            definition_id: definition_id.clone(),
            from_version,
            to_version,
            changed_steps,
            policy_changed: from.pinned_policy_digest() != to.pinned_policy_digest(),
            configuration_changed: from.pinned_configuration_digest()
                != to.pinned_configuration_digest(),
            catalog_changed: from.pinned_catalog_digest() != to.pinned_catalog_digest(),
        })
    }
}

pub fn prepare_workflow_definition_registration(
    context: &RequestContext,
    definition: WorkflowDefinition,
) -> Result<WorkflowDefinition, WorkflowCoordinationError> {
    if definition.project_id() != &context.scope().project_id {
        return Err(WorkflowCoordinationError::ScopeMismatch);
    }
    definition
        .validate()
        .map_err(|_| WorkflowCoordinationError::InvalidDefinition)?;
    Ok(definition)
}

fn coordination_authority_error(
    error: WorkflowDefinitionAuthorityError,
) -> WorkflowCoordinationError {
    match error {
        WorkflowDefinitionAuthorityError::AlreadyExists => {
            WorkflowCoordinationError::ImmutableDefinitionConflict
        }
        WorkflowDefinitionAuthorityError::Conflict => {
            WorkflowCoordinationError::ImmutableDefinitionConflict
        }
        WorkflowDefinitionAuthorityError::Unavailable(message) => {
            WorkflowCoordinationError::AuthorityUnavailable(message)
        }
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
pub struct TaskHandoffScope {
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

impl TaskHandoffScope {
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

impl<'de> Deserialize<'de> for TaskHandoffScope {
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
pub struct TaskHandoffGrant {
    scope: TaskHandoffScope,
    token_digest: ManifestDigest,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    /// The exact work/evidence frontier this handoff records (Plan 24).
    frontier: WorkHandoffFrontierV1,
    /// The canonical digest of `frontier`; lineage chains hold this value.
    frontier_digest: ManifestDigest,
}

impl TaskHandoffGrant {
    pub fn new(
        scope: TaskHandoffScope,
        token_digest: ManifestDigest,
        issued_at: UtcMicros,
        expires_at: UtcMicros,
        frontier: WorkHandoffFrontierV1,
    ) -> Result<Self, TaskHandoffError> {
        let frontier_digest = frontier
            .digest()
            .map_err(|_| TaskHandoffError::InvalidFrontier)?;
        let grant = Self {
            scope,
            token_digest,
            issued_at,
            expires_at,
            frontier,
            frontier_digest,
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
        if lifetime_micros != TASK_HANDOFF_LIFETIME_MICROS.0 {
            return Err(TaskHandoffError::InvalidExpiry);
        }
        // The frontier is bound to exactly the handed-off task and to the
        // actor doing the handing off; a frontier for another task or from
        // another issuer is not this grant's checkpoint evidence.
        if self.frontier.task_id() != self.scope.task_id()
            || self.frontier.lineage().issued_by != *self.scope.from_actor_id()
        {
            return Err(TaskHandoffError::InvalidFrontier);
        }
        let digest = self
            .frontier
            .digest()
            .map_err(|_| TaskHandoffError::InvalidFrontier)?;
        if digest != self.frontier_digest {
            return Err(TaskHandoffError::InvalidFrontier);
        }
        Ok(())
    }

    pub fn scope(&self) -> &TaskHandoffScope {
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

    pub fn frontier(&self) -> &WorkHandoffFrontierV1 {
        &self.frontier
    }

    pub fn frontier_digest(&self) -> &ManifestDigest {
        &self.frontier_digest
    }
}

impl<'de> Deserialize<'de> for TaskHandoffGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            scope: TaskHandoffScope,
            token_digest: ManifestDigest,
            issued_at: UtcMicros,
            expires_at: UtcMicros,
            frontier: WorkHandoffFrontierV1,
            frontier_digest: ManifestDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        let grant = Self::new(
            wire.scope,
            wire.token_digest,
            wire.issued_at,
            wire.expires_at,
            wire.frontier,
        )
        .map_err(serde::de::Error::custom)?;
        if grant.frontier_digest != wire.frontier_digest {
            return Err(serde::de::Error::custom(TaskHandoffError::InvalidFrontier));
        }
        Ok(grant)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TaskHandoffConsumeOutcome {
    /// Consumed exactly once; the stored frontier travels with the
    /// consumption so the redeemer receives the recorded checkpoint.
    Consumed {
        frontier: Box<WorkHandoffFrontierV1>,
    },
    Missing,
    ScopeMismatch,
    Expired,
    Replay,
}

/// Wire request for [`TaskHandoffService::issue`].
///
/// `secret` is the caller-supplied bearer token; the authority persists only
/// its digest, never the secret itself.
#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffIssueRequest {
    pub scope: TaskHandoffScope,
    pub secret: String,
    /// The exact work/evidence frontier this handoff records (Plan 24).
    pub frontier: WorkHandoffFrontierV1,
}

impl fmt::Debug for TaskHandoffIssueRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandoffIssueRequest")
            .field("scope", &self.scope)
            .field("secret", &"[REDACTED]")
            .field("frontier", &self.frontier)
            .finish()
    }
}

/// Wire request for [`TaskHandoffService::redeem`].
#[derive(Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffRedeemRequest {
    pub secret: String,
    pub expected_scope: TaskHandoffScope,
}

impl fmt::Debug for TaskHandoffRedeemRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskHandoffRedeemRequest")
            .field("secret", &"[REDACTED]")
            .field("expected_scope", &self.expected_scope)
            .finish()
    }
}

/// Wire response for [`TaskHandoffService::redeem`]: the redemption receipt,
/// once and only once, for the caller that actually consumed the grant.
///
/// The receipt is checkpoint evidence only. It deliberately carries no
/// lease, fence, or acceptance authority: redeeming a handoff cannot renew
/// a lease, establish task acceptance, or mutate graph or runtime state
/// (Plan 24) — the redeemer must earn runtime authority through the normal
/// admission and lease paths.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoffRedeemed {
    pub scope: TaskHandoffScope,
    /// The frontier exactly as the issuer recorded it.
    pub frontier: WorkHandoffFrontierV1,
    /// The canonical digest of `frontier`, for lineage chaining.
    pub frontier_digest: ManifestDigest,
    pub redeemed_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskHandoffAuthorityError {
    Conflict,
    Unavailable(String),
}

pub trait TaskHandoffAuthorityPort: Send + Sync {
    fn issue(&self, grant: &TaskHandoffGrant) -> Result<(), TaskHandoffAuthorityError>;

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected_scope: &TaskHandoffScope,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskHandoffError {
    InvalidToken,
    InvalidScope,
    InvalidFrontier,
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
            Self::InvalidFrontier => {
                formatter.write_str("task handoff frontier record is invalid for this scope")
            }
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
        context: &RequestContext,
        scope: TaskHandoffScope,
        token: &TaskHandoffToken,
        issued_at: UtcMicros,
        frontier: WorkHandoffFrontierV1,
    ) -> Result<TaskHandoffGrant, TaskHandoffError> {
        let grant = prepare_task_handoff_issue(context, scope, token, issued_at, frontier)?;
        self.authority
            .issue(&grant)
            .map_err(handoff_authority_error)?;
        Ok(grant)
    }

    /// Consumes the grant once and answers the redemption receipt.
    ///
    /// The receipt carries the recorded frontier and nothing else: this path
    /// holds no lease authority and touches no attempt, projection, or graph
    /// state, so a redeemed handoff can never renew a lease or stand in for
    /// task acceptance.
    pub fn redeem(
        &self,
        context: &RequestContext,
        token: &TaskHandoffToken,
        expected_scope: &TaskHandoffScope,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffRedeemed, TaskHandoffError> {
        let token_digest = prepare_task_handoff_redeem(context, token, expected_scope)?;
        match self
            .authority
            .consume(&token_digest, expected_scope, consumed_at)
            .map_err(handoff_authority_error)?
        {
            TaskHandoffConsumeOutcome::Consumed { frontier } => {
                let frontier_digest = frontier
                    .digest()
                    .map_err(|_| TaskHandoffError::InvalidFrontier)?;
                Ok(TaskHandoffRedeemed {
                    scope: expected_scope.clone(),
                    frontier: *frontier,
                    frontier_digest,
                    redeemed_at: consumed_at,
                })
            }
            TaskHandoffConsumeOutcome::Missing => Err(TaskHandoffError::Missing),
            TaskHandoffConsumeOutcome::ScopeMismatch => Err(TaskHandoffError::ScopeMismatch),
            TaskHandoffConsumeOutcome::Expired => Err(TaskHandoffError::Expired),
            TaskHandoffConsumeOutcome::Replay => Err(TaskHandoffError::Replay),
        }
    }
}

pub fn prepare_task_handoff_issue(
    context: &RequestContext,
    scope: TaskHandoffScope,
    token: &TaskHandoffToken,
    issued_at: UtcMicros,
    frontier: WorkHandoffFrontierV1,
) -> Result<TaskHandoffGrant, TaskHandoffError> {
    if !handoff_scope_matches_context(context, &scope) || context.actor() != scope.from_actor_id() {
        return Err(TaskHandoffError::Unauthorized);
    }
    let expires_at = UtcMicros(
        issued_at
            .0
            .checked_add(TASK_HANDOFF_LIFETIME_MICROS.0)
            .ok_or(TaskHandoffError::InvalidExpiry)?,
    );
    TaskHandoffGrant::new(scope, token.digest()?, issued_at, expires_at, frontier)
}

pub fn prepare_task_handoff_redeem(
    context: &RequestContext,
    token: &TaskHandoffToken,
    expected_scope: &TaskHandoffScope,
) -> Result<ManifestDigest, TaskHandoffError> {
    expected_scope.validate()?;
    if !handoff_scope_matches_context(context, expected_scope)
        || context.actor() != expected_scope.to_actor_id()
    {
        return Err(TaskHandoffError::Unauthorized);
    }
    token.digest()
}

fn handoff_scope_matches_context(context: &RequestContext, scope: &TaskHandoffScope) -> bool {
    scope.project_id() == &context.scope().project_id
        && scope.repository_id() == &context.scope().repository_id
        && scope.worktree_id() == &context.scope().worktree_id
}

fn handoff_authority_error(error: TaskHandoffAuthorityError) -> TaskHandoffError {
    match error {
        TaskHandoffAuthorityError::Conflict => TaskHandoffError::Conflict,
        TaskHandoffAuthorityError::Unavailable(message) => {
            TaskHandoffError::AuthorityUnavailable(message)
        }
    }
}
