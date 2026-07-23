//! Closed, repository-specific read operations and results admitted by the
//! runtime read port.
//!
//! These enums mirror [`RepositoryWritePayloadV1`](crate::RepositoryWritePayloadV1):
//! store-owned, driver-neutral, and typed over validated store/domain DTOs.
//! There is intentionally no query string, untyped JSON value, byte blob, or
//! generic command variant. Adding a repository read therefore requires adding
//! a typed store projection first.
//!
//! The concrete SQLite executors that answer these operations live in the
//! `tracedecay-rusqlite-runtime` crate; this module owns only the contract.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalObservationIdV1, CodeGenerationId, ConfigurationRevisionId, DurableObservationV1,
    FactLineageEventV1, FileOccurrenceId, GenerationDiagnosticV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceIdentityV1, RetrievalAnchorId, SessionId,
    SessionProjectionGenerationV1, SessionSummaryIdV1, SessionSummaryRecordV1,
};

use crate::{
    ConfigurationRevisionRecordV1, FactCurrentQuery, FactLineageQuery,
    SessionTemporalProjectionBatchV1, StoredFactV1,
};

/// One repository read operation, dispatched across the profile, project, and
/// session families.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadOperationV1 {
    Profile(ProfileReadOperationV1),
    Project(ProjectReadOperationV1),
    Session(SessionReadOperationV1),
}

/// One repository read result, mirroring [`RepositoryReadOperationV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadResultV1 {
    Profile(ProfileReadResultV1),
    Project(Box<ProjectReadResultV1>),
    Session(SessionReadResultV1),
}

/// Profile-family (configuration) read operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileReadOperationV1 {
    CurrentConfiguration,
    ConfigurationRevision(ConfigurationRevisionId),
}

/// Profile-family (configuration) read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileReadResultV1 {
    ConfigurationRevision(Option<ConfigurationRevisionRecordV1>),
}

/// Project-family read operations across facts, observations, and diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectReadOperationV1 {
    Fact(FactReadOperationV1),
    Observation(ObservationReadOperationV1),
    Diagnostics(DiagnosticReadOperationV1),
}

/// Project-family read results across facts, observations, and diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectReadResultV1 {
    Fact(FactReadResultV1),
    Observation(ObservationReadResultV1),
    Diagnostics(DiagnosticReadResultV1),
}

/// Fact-family read operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactReadOperationV1 {
    Current(FactCurrentQuery),
    Lineage(FactLineageQuery),
}

/// Fact-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactReadResultV1 {
    Current(Box<Option<StoredFactV1>>),
    Lineage(Vec<FactLineageEventV1>),
}

/// Observation-family read operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationReadOperationV1 {
    SourceCursor {
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
    },
    Observation {
        observation_id: CanonicalObservationIdV1,
    },
}

/// One stored observation row projected with its commit sequence and cursor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredObservationRowV1 {
    pub sequence: u64,
    pub observation: DurableObservationV1,
    pub committed_cursor: ObservationSourceCursorV1,
}

/// Observation-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationReadResultV1 {
    SourceCursor(Option<ObservationSourceCursorV1>),
    Observation(Box<Option<StoredObservationRowV1>>),
}

/// Diagnostic-family read operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticReadOperationV1 {
    CurrentGeneration,
    Generation(CodeGenerationId),
    CurrentForFile {
        generation_id: CodeGenerationId,
        file_occurrence_id: FileOccurrenceId,
    },
    ByAnchor(RetrievalAnchorId),
}

/// Diagnostic-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticReadResultV1 {
    CurrentGeneration(Option<CodeGenerationId>),
    Records(Vec<GenerationDiagnosticV1>),
    Record(Box<Option<GenerationDiagnosticV1>>),
}

/// Session-family read operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionReadOperationV1 {
    ProjectionBatch {
        session_id: SessionId,
        generation: SessionProjectionGenerationV1,
        batch_ordinal: u64,
    },
    Summary(SessionSummaryIdV1),
}

/// Session-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionReadResultV1 {
    ProjectionBatch(Option<SessionTemporalProjectionBatchV1>),
    Summary(Option<SessionSummaryRecordV1>),
}
