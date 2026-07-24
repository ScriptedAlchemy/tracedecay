use rusqlite::{Savepoint, Transaction};
use tracedecay_store::{
    AnchoredObservationWrite, EvidenceAssemblyWriteV1, FactWriteBatch, ObservationCursorAdvance,
    ProjectReadOperationV1, ProjectReadResultV1, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, SanitizedCleanDiagnosticSnapshotV1,
};

use super::{
    DiagnosticExecutor, EvidenceAssemblyExecutor, FactExecutor, ObservationExecutor,
    RetrievalAnchorExecutor,
};

#[derive(Clone, Default)]
pub struct ProjectExecutor {
    fact: FactExecutor,
    observation: ObservationExecutor,
    diagnostics: DiagnosticExecutor,
    evidence_assembly: EvidenceAssemblyExecutor,
    retrieval_anchor: RetrievalAnchorExecutor,
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
        write: &AnchoredObservationWrite,
    ) -> rusqlite::Result<()> {
        self.observation.execute_write(savepoint, write)
    }

    pub fn execute_observation_cursor_advance(
        &mut self,
        savepoint: &Savepoint<'_>,
        advance: &ObservationCursorAdvance,
    ) -> rusqlite::Result<()> {
        self.observation.execute_cursor_advance(savepoint, advance)
    }

    pub fn execute_diagnostic_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        snapshot: &SanitizedCleanDiagnosticSnapshotV1,
    ) -> rusqlite::Result<()> {
        self.diagnostics.execute_write(savepoint, snapshot)
    }

    pub fn execute_evidence_assembly_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        write: &EvidenceAssemblyWriteV1,
    ) -> rusqlite::Result<()> {
        self.evidence_assembly.execute_write(savepoint, write)
    }

    pub fn execute_retrieval_anchor_disposition_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        record: &RetrievalAnchorDispositionRecordV1,
    ) -> rusqlite::Result<()> {
        self.retrieval_anchor
            .execute_disposition_write(savepoint, record)
    }

    pub fn execute_retrieval_anchor_derivative_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        derivative: &RetrievalAnchorDerivativeV1,
    ) -> rusqlite::Result<()> {
        self.retrieval_anchor
            .execute_derivative_write(savepoint, derivative)
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
            ProjectReadOperationV1::EvidenceAssembly(operation) => self
                .evidence_assembly
                .execute_read(snapshot, operation)
                .map(ProjectReadResultV1::EvidenceAssembly),
            ProjectReadOperationV1::RetrievalAnchor(operation) => self
                .retrieval_anchor
                .execute_read(snapshot, operation)
                .map(ProjectReadResultV1::RetrievalAnchor),
        }
    }
}
