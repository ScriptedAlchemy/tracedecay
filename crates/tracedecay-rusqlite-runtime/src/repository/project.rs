use rusqlite::{Savepoint, Transaction};
use tracedecay_store::{FactWriteBatch, ObservationWrite, SanitizedCleanDiagnosticSnapshotV1};

use super::{
    DiagnosticExecutor, DiagnosticReadOperationV1, DiagnosticReadResultV1, FactExecutor,
    FactReadOperationV1, FactReadResultV1, ObservationExecutor, ObservationReadOperationV1,
    ObservationReadResultV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectReadOperationV1 {
    Fact(FactReadOperationV1),
    Observation(ObservationReadOperationV1),
    Diagnostics(DiagnosticReadOperationV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectReadResultV1 {
    Fact(FactReadResultV1),
    Observation(ObservationReadResultV1),
    Diagnostics(DiagnosticReadResultV1),
}

#[derive(Clone, Default)]
pub struct ProjectExecutor {
    fact: FactExecutor,
    observation: ObservationExecutor,
    diagnostics: DiagnosticExecutor,
}

impl ProjectExecutor {
    pub fn execute_fact_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        batch: &FactWriteBatch,
    ) -> rusqlite::Result<()> {
        self.fact.execute_write(savepoint, batch)
    }

    pub fn execute_observation_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        write: &ObservationWrite,
    ) -> rusqlite::Result<()> {
        self.observation.execute_write(savepoint, write)
    }

    pub fn execute_diagnostic_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        snapshot: &SanitizedCleanDiagnosticSnapshotV1,
    ) -> rusqlite::Result<()> {
        self.diagnostics.execute_write(savepoint, snapshot)
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &ProjectReadOperationV1,
    ) -> rusqlite::Result<ProjectReadResultV1> {
        match operation {
            ProjectReadOperationV1::Fact(operation) => self
                .fact
                .execute_read(snapshot, operation)
                .map(ProjectReadResultV1::Fact),
            ProjectReadOperationV1::Observation(operation) => self
                .observation
                .execute_read(snapshot, operation)
                .map(ProjectReadResultV1::Observation),
            ProjectReadOperationV1::Diagnostics(operation) => self
                .diagnostics
                .execute_read(snapshot, operation)
                .map(ProjectReadResultV1::Diagnostics),
        }
    }
}
