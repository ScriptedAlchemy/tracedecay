//! Concrete pre-cutover adapters over already-open canonical SQLite shards.
//!
//! The executors contain no locator, opener, migration installer, registry
//! binding, cutover selector, or generic SQL surface. The daemon may mount the
//! exported bundle only after a later cutover stage supplies an authorized
//! runtime attachment.

mod attachment;
mod configuration;
mod diagnostics;
mod fact;
mod fixtures;
mod observation;
mod project;
mod session;
mod support;

use rusqlite::{Savepoint, Transaction};
use tracedecay_store::RepositoryWritePayloadV1;

use crate::StorageOperationExecutor;

pub use attachment::{
    RepositoryAttachmentStartError, RepositoryDispatchError, RepositoryPhysicalAttachmentFactory,
    RepositoryRuntimePhysicalAttachment, RepositoryRuntimePhysicalSnapshot,
};
pub use configuration::{ConfigurationExecutor, ProfileReadOperationV1, ProfileReadResultV1};
pub use diagnostics::{DiagnosticExecutor, DiagnosticReadOperationV1, DiagnosticReadResultV1};
pub use fact::{FactExecutor, FactReadOperationV1, FactReadResultV1};
pub use fixtures::{AdapterParityFixtureV1, PRE_CUTOVER_ADAPTER_PARITY_FIXTURES_V1};
pub use observation::{
    ObservationExecutor, ObservationReadOperationV1, ObservationReadResultV1,
    StoredObservationRowV1,
};
pub use project::{ProjectExecutor, ProjectReadOperationV1, ProjectReadResultV1};
pub use session::{SessionExecutor, SessionReadOperationV1, SessionReadResultV1};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryReadOperationV1 {
    Profile(ProfileReadOperationV1),
    Project(ProjectReadOperationV1),
    Session(SessionReadOperationV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryReadResultV1 {
    Profile(ProfileReadResultV1),
    Project(Box<ProjectReadResultV1>),
    Session(SessionReadResultV1),
}

#[derive(Default)]
pub struct ConcreteRepositoryWriteExecutor {
    configuration: ConfigurationExecutor,
    project: ProjectExecutor,
    session: SessionExecutor,
}

impl StorageOperationExecutor for ConcreteRepositoryWriteExecutor {
    fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        match payload {
            RepositoryWritePayloadV1::Configuration(commit) => {
                self.configuration.execute_write(savepoint, commit)
            }
            RepositoryWritePayloadV1::Fact(batch) => {
                self.project.execute_fact_write(savepoint, batch)
            }
            RepositoryWritePayloadV1::Observation(write) => {
                self.project.execute_observation_write(savepoint, write)
            }
            RepositoryWritePayloadV1::Diagnostics(snapshot) => {
                self.project.execute_diagnostic_write(savepoint, snapshot)
            }
            RepositoryWritePayloadV1::SessionProjection(batch) => {
                self.session.execute_projection_write(savepoint, batch)
            }
            RepositoryWritePayloadV1::SessionSummary(request) => {
                self.session.execute_summary_write(savepoint, request)
            }
            RepositoryWritePayloadV1::GitIndexTransaction(_)
            | RepositoryWritePayloadV1::EnqueueOutbox(_)
            | RepositoryWritePayloadV1::ApplyInbox(_)
            | RepositoryWritePayloadV1::AcknowledgeOutbox(_) => {
                Err(rusqlite::Error::InvalidParameterName(format!(
                    "repository attachment does not own {}",
                    payload.name()
                )))
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct ConcreteRepositoryReadExecutor {
    configuration: ConfigurationExecutor,
    project: ProjectExecutor,
    session: SessionExecutor,
}

impl ConcreteRepositoryReadExecutor {
    pub fn execute(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &RepositoryReadOperationV1,
    ) -> rusqlite::Result<RepositoryReadResultV1> {
        match operation {
            RepositoryReadOperationV1::Profile(operation) => self
                .configuration
                .execute_read(snapshot, operation)
                .map(RepositoryReadResultV1::Profile),
            RepositoryReadOperationV1::Project(operation) => self
                .project
                .execute_read(snapshot, operation)
                .map(|result| RepositoryReadResultV1::Project(Box::new(result))),
            RepositoryReadOperationV1::Session(operation) => self
                .session
                .execute_read(snapshot, operation)
                .map(RepositoryReadResultV1::Session),
        }
    }
}

/// One inert attachment bundle for the later registry-mount stage.
///
/// Constructing this value does not open a database or bind any authority.
#[derive(Default)]
pub struct PreCutoverRepositoryAttachmentBundle {
    pub write: ConcreteRepositoryWriteExecutor,
    pub read: ConcreteRepositoryReadExecutor,
}

impl PreCutoverRepositoryAttachmentBundle {
    pub fn new() -> Self {
        Self::default()
    }
}
