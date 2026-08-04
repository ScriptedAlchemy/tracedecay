//! Concrete adapters over already-open canonical SQLite shards.
//!
//! These executors are mounted in production. The daemon's store-runtime
//! registry attaches every non-code shard through
//! [`RepositoryPhysicalAttachmentFactory`], which builds a
//! [`ConcreteRepositoryWriteExecutor`] and a
//! [`ConcreteRepositoryReadExecutor`]; see
//! `crates/tracedecay-runtime-core/src/store_runtime/registry/ports.rs`. The
//! executors still contain no
//! locator, opener, migration installer, registry binding, or
//! generic SQL surface — the attachment supplies all of those.
//!
//! Which operations are live is a separate question from whether the executors
//! are mounted. Every payload and read operation an application actually
//! constructs today routes through here: facts, observations and cursor
//! advances, diagnostics, evidence assembly, external sources, retrieval-anchor
//! dispositions and derivatives, and session projections and summaries. Three
//! surfaces are wired and tested but not yet constructed by any production
//! caller, and are retained as the landing zone for their migration:
//!
//! - the profile/configuration family
//!   ([`RepositoryWritePayloadV1::Configuration`] and every
//!   [`RepositoryReadOperationV1::Profile`] operation), whose live writer is
//!   still `crates/tracedecay-global-db/src/configuration/store.rs`;
//! - [`RepositoryWritePayloadV1::DiagnosticSupersession`] and the
//!   `Stale`/`SupersessionChain` diagnostic reads, whose live engine is still
//!   `src/diagnostics_store.rs`;
//!
//! `Code` operations cross the graph-db boundary, while `Effects` operations
//! are owned by the writer ledger; both dispatch arms here reject them.

mod attachment;
mod configuration;
mod diagnostics;
pub(crate) mod evidence_assembly;
mod external_source;
mod fact;
mod observation;
mod project;
mod retrieval_anchor;
mod scope_set;
mod session;
mod support;

use rusqlite::{Savepoint, Transaction};
use tracedecay_store::RepositoryWritePayloadV1;

use crate::StorageOperationExecutor;

pub use attachment::{
    RepositoryAttachmentStartError, RepositoryDispatchError, RepositoryPhysicalAttachmentFactory,
    RepositoryRuntimePhysicalAttachment, RepositoryRuntimePhysicalSnapshot,
};
pub use configuration::ConfigurationExecutor;
pub use diagnostics::DiagnosticExecutor;
pub use evidence_assembly::EvidenceAssemblyExecutor;
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub use evidence_assembly::tests::write_fixture_for_project;
pub use external_source::{EXTERNAL_SOURCE_SCHEMA_V1, ExternalSourceExecutor};
pub use fact::FactExecutor;
pub use observation::ObservationExecutor;
pub use project::ProjectExecutor;
pub use retrieval_anchor::RetrievalAnchorExecutor;
pub use scope_set::{
    AUTHORIZED_SCOPE_SET_SCHEMA_V1, AuthorizedScopeSetExecutor, AuthorizedScopeSetSqliteStorage,
    AuthorizedScopeSetStoreError,
};
pub use session::SessionExecutor;

// The read operation/result contract now lives in `tracedecay-store`. Re-export
// the moved types so existing `repository::` paths keep resolving across the
// workspace.
pub use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, DiagnosticReadOperationV1, DiagnosticReadResultV1,
    EffectsReadOperationV1, EffectsReadResultV1, ExternalSourceReadOperationV1,
    ExternalSourceReadResultV1, FactReadOperationV1, FactReadResultV1, ObservationReadOperationV1,
    ObservationReadResultV1, ProfileReadOperationV1, ProfileReadResultV1, ProjectReadOperationV1,
    ProjectReadResultV1, RepositoryReadOperationV1, RepositoryReadResultV1, SessionReadOperationV1,
    SessionReadResultV1, StoredObservationRowV1,
};

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
            RepositoryWritePayloadV1::ObservationCursorAdvance(advance) => self
                .project
                .execute_observation_cursor_advance(savepoint, advance),
            RepositoryWritePayloadV1::Diagnostics(snapshot) => {
                self.project.execute_diagnostic_write(savepoint, snapshot)
            }
            RepositoryWritePayloadV1::DiagnosticSupersession(request) => self
                .project
                .execute_diagnostic_supersession(savepoint, request),
            RepositoryWritePayloadV1::EvidenceAssembly(write) => self
                .project
                .execute_evidence_assembly_write(savepoint, write),
            RepositoryWritePayloadV1::ExternalSource(commit) => self
                .project
                .execute_external_source_write(savepoint, commit),
            RepositoryWritePayloadV1::RetrievalAnchorDisposition(record) => self
                .project
                .execute_retrieval_anchor_disposition_write(savepoint, record),
            RepositoryWritePayloadV1::RetrievalAnchorDerivative(derivative) => self
                .project
                .execute_retrieval_anchor_derivative_write(savepoint, derivative),
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
            RepositoryReadOperationV1::ExternalSource(operation) => self
                .project
                .execute_external_source_read(snapshot, operation)
                .map(RepositoryReadResultV1::ExternalSource),
            RepositoryReadOperationV1::Session(operation) => self
                .session
                .execute_read(snapshot, operation)
                .map(RepositoryReadResultV1::Session),
            RepositoryReadOperationV1::Code(_) => Err(rusqlite::Error::InvalidParameterName(
                "repository attachment does not own code reads".to_owned(),
            )),
            RepositoryReadOperationV1::Effects(_) => Err(rusqlite::Error::InvalidParameterName(
                "repository attachment does not own effects reads".to_owned(),
            )),
        }
    }
}
