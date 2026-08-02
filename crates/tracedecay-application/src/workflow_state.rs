//! Transport-neutral workflow aggregate mutation services and persistence ports.

use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_domain::workflow_state::{
    WorkflowAggregateVersionV1, WorkflowCommandContextV1, WorkflowDefinitionCommandV1,
    WorkflowDefinitionEventV1, WorkflowDefinitionProjectionV1, WorkflowRunCommandV1,
    WorkflowRunEventV1, WorkflowRunProjectionV1, WorkflowStateError,
};
use tracedecay_domain::{RunId, WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowStepId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowAggregateAppendRequest<E> {
    pub expected_version: Option<WorkflowAggregateVersionV1>,
    pub event: E,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowAggregateSnapshot<P, E> {
    pub projection: P,
    pub events: Vec<E>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowAppendOutcome<P> {
    Appended(P),
    Replayed(P),
}

impl<P> WorkflowAppendOutcome<P> {
    pub fn into_projection(self) -> P {
        match self {
            Self::Appended(projection) | Self::Replayed(projection) => projection,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStateStoreError {
    #[error("workflow aggregate was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("workflow aggregate version changed")]
    VersionConflict,
    #[error("workflow command identity was reused with different input")]
    IdempotencyConflict,
    #[error("workflow aggregate history is invalid")]
    InvalidHistory(WorkflowStateError),
    #[error("workflow aggregate storage is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkflowStateApplicationError {
    #[error("workflow aggregate was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("workflow aggregate version changed")]
    VersionConflict,
    #[error("workflow command identity was reused with different input")]
    IdempotencyConflict,
    #[error("workflow command is invalid")]
    InvalidCommand(WorkflowStateError),
    #[error("workflow aggregate history is invalid")]
    InvalidHistory(WorkflowStateError),
    #[error("workflow aggregate storage is unavailable")]
    Unavailable,
}

pub trait WorkflowDefinitionEventStorePort: Send + Sync {
    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowDefinitionProjectionV1, WorkflowDefinitionEventV1>,
        WorkflowStateStoreError,
    >;

    fn history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionEventV1>, WorkflowStateStoreError>;

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowDefinitionEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowDefinitionProjectionV1>, WorkflowStateStoreError>;
}

pub trait WorkflowRunEventStorePort: Send + Sync {
    fn load(
        &self,
        run_id: &RunId,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowRunProjectionV1, WorkflowRunEventV1>,
        WorkflowStateStoreError,
    >;

    fn history(&self, run_id: &RunId) -> Result<Vec<WorkflowRunEventV1>, WorkflowStateStoreError>;

    fn append(
        &self,
        request: &WorkflowAggregateAppendRequest<WorkflowRunEventV1>,
    ) -> Result<WorkflowAppendOutcome<WorkflowRunProjectionV1>, WorkflowStateStoreError>;
}

pub struct WorkflowDefinitionAggregateService<P> {
    store: P,
}

impl<P> WorkflowDefinitionAggregateService<P>
where
    P: WorkflowDefinitionEventStorePort,
{
    pub const fn new(store: P) -> Self {
        Self { store }
    }

    pub fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowDefinitionProjectionV1, WorkflowDefinitionEventV1>,
        WorkflowStateApplicationError,
    > {
        self.store
            .load(definition_id, definition_version)
            .map_err(application_store_error)
    }

    pub fn history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionEventV1>, WorkflowStateApplicationError> {
        self.store
            .history(definition_id, definition_version)
            .map_err(application_store_error)
    }

    pub fn register(
        &self,
        definition: WorkflowDefinitionV1,
        approval_steps: BTreeSet<WorkflowStepId>,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowDefinitionProjectionV1, WorkflowStateApplicationError> {
        let projection =
            WorkflowDefinitionProjectionV1::register(definition, approval_steps, context)
                .map_err(WorkflowStateApplicationError::InvalidCommand)?;
        let event = latest_definition_event(&projection)?;
        self.store
            .append(&WorkflowAggregateAppendRequest {
                expected_version: None,
                event,
            })
            .map(WorkflowAppendOutcome::into_projection)
            .map_err(application_store_error)
    }

    pub fn validate(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowDefinitionProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            definition_id,
            definition_version,
            expected_version,
            WorkflowDefinitionCommandV1::Validate,
            context,
        )
    }

    pub fn activate(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowDefinitionProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            definition_id,
            definition_version,
            expected_version,
            WorkflowDefinitionCommandV1::Activate,
            context,
        )
    }

    pub fn retire(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowDefinitionProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            definition_id,
            definition_version,
            expected_version,
            WorkflowDefinitionCommandV1::Retire,
            context,
        )
    }

    fn mutate(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
        expected_version: WorkflowAggregateVersionV1,
        command: WorkflowDefinitionCommandV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowDefinitionProjectionV1, WorkflowStateApplicationError> {
        let current = self
            .store
            .load(definition_id, definition_version)
            .map_err(application_store_error)?
            .projection;
        if let Some(admitted) = current
            .history()
            .iter()
            .find(|event| event.command_id() == context.command_id())
        {
            return if admitted.input_digest() == context.input_digest() {
                Ok(current)
            } else {
                Err(WorkflowStateApplicationError::IdempotencyConflict)
            };
        }
        let next = current
            .handle(command, context)
            .map_err(WorkflowStateApplicationError::InvalidCommand)?;
        let event = latest_definition_event(&next)?;
        self.store
            .append(&WorkflowAggregateAppendRequest {
                expected_version: Some(expected_version),
                event,
            })
            .map(WorkflowAppendOutcome::into_projection)
            .map_err(application_store_error)
    }
}

pub struct WorkflowRunAggregateService<P> {
    store: P,
}

impl<P> WorkflowRunAggregateService<P>
where
    P: WorkflowRunEventStorePort,
{
    pub const fn new(store: P) -> Self {
        Self { store }
    }

    pub fn load(
        &self,
        run_id: &RunId,
    ) -> Result<
        WorkflowAggregateSnapshot<WorkflowRunProjectionV1, WorkflowRunEventV1>,
        WorkflowStateApplicationError,
    > {
        self.store.load(run_id).map_err(application_store_error)
    }

    pub fn history(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<WorkflowRunEventV1>, WorkflowStateApplicationError> {
        self.store.history(run_id).map_err(application_store_error)
    }

    pub fn admit(
        &self,
        run_id: RunId,
        definition: &WorkflowDefinitionProjectionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        let projection = WorkflowRunProjectionV1::start(run_id, definition, context)
            .map_err(WorkflowStateApplicationError::InvalidCommand)?;
        let event = latest_run_event(&projection)?;
        self.store
            .append(&WorkflowAggregateAppendRequest {
                expected_version: None,
                event,
            })
            .map(WorkflowAppendOutcome::into_projection)
            .map_err(application_store_error)
    }

    pub fn start(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::StartStep { step_id },
            context,
        )
    }

    pub fn succeed(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::CompleteStep { step_id },
            context,
        )
    }

    pub fn fail(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::FailStep { step_id },
            context,
        )
    }

    pub fn request_approval(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::RequestApproval { step_id },
            context,
        )
    }

    pub fn approve(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::RecordApproval { step_id },
            context,
        )
    }

    pub fn deny(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::RecordDenial { step_id },
            context,
        )
    }

    pub fn pause(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::Pause,
            context,
        )
    }

    pub fn resume(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::Resume,
            context,
        )
    }

    pub fn cancel(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::RequestCancellation,
            context,
        )
    }

    pub fn restart(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::Restart,
            context,
        )
    }

    pub fn reconcile(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        step_id: WorkflowStepId,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        self.mutate(
            run_id,
            expected_version,
            WorkflowRunCommandV1::ReconcileStepCancelled { step_id },
            context,
        )
    }

    fn mutate(
        &self,
        run_id: &RunId,
        expected_version: WorkflowAggregateVersionV1,
        command: WorkflowRunCommandV1,
        context: WorkflowCommandContextV1,
    ) -> Result<WorkflowRunProjectionV1, WorkflowStateApplicationError> {
        let current = self
            .store
            .load(run_id)
            .map_err(application_store_error)?
            .projection;
        if let Some(admitted) = current
            .history()
            .iter()
            .find(|event| event.command_id() == context.command_id())
        {
            return if admitted.input_digest() == context.input_digest() {
                Ok(current)
            } else {
                Err(WorkflowStateApplicationError::IdempotencyConflict)
            };
        }
        let next = current
            .handle(command, context)
            .map_err(WorkflowStateApplicationError::InvalidCommand)?;
        let event = latest_run_event(&next)?;
        self.store
            .append(&WorkflowAggregateAppendRequest {
                expected_version: Some(expected_version),
                event,
            })
            .map(WorkflowAppendOutcome::into_projection)
            .map_err(application_store_error)
    }
}

fn latest_definition_event(
    projection: &WorkflowDefinitionProjectionV1,
) -> Result<WorkflowDefinitionEventV1, WorkflowStateApplicationError> {
    projection
        .history()
        .last()
        .cloned()
        .ok_or(WorkflowStateApplicationError::InvalidCommand(
            WorkflowStateError::EmptyHistory,
        ))
}

fn latest_run_event(
    projection: &WorkflowRunProjectionV1,
) -> Result<WorkflowRunEventV1, WorkflowStateApplicationError> {
    projection
        .history()
        .last()
        .cloned()
        .ok_or(WorkflowStateApplicationError::InvalidCommand(
            WorkflowStateError::EmptyHistory,
        ))
}

fn application_store_error(error: WorkflowStateStoreError) -> WorkflowStateApplicationError {
    match error {
        WorkflowStateStoreError::NotFoundOrNotAuthorized => {
            WorkflowStateApplicationError::NotFoundOrNotAuthorized
        }
        WorkflowStateStoreError::VersionConflict => WorkflowStateApplicationError::VersionConflict,
        WorkflowStateStoreError::IdempotencyConflict => {
            WorkflowStateApplicationError::IdempotencyConflict
        }
        WorkflowStateStoreError::InvalidHistory(error) => {
            WorkflowStateApplicationError::InvalidHistory(error)
        }
        WorkflowStateStoreError::Unavailable => WorkflowStateApplicationError::Unavailable,
    }
}
