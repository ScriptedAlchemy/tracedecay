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
    FactLineageEventV1, FileOccurrenceId, GenerationDiagnosticV1, GitIndexIdempotencyKey,
    GitIndexPreviewId, GitIndexPreviewV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceIdentityV1, RepositoryId, RetrievalAnchorId, SessionId,
    SessionProjectionGenerationV1, SessionSummaryIdV1, SessionSummaryRecordV1,
};

use crate::{
    ConfigurationRevisionRecordV1, FactCurrentQuery, FactLineageQuery, GitIndexTransactionRecordV1,
    SessionTemporalProjectionBatchV1, StoreEffectIdV1, StoreRuntimeBindingV1, StoredFactV1,
    TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

/// One repository read operation, dispatched across the profile, project,
/// session, code, and effects families.
///
/// This enum mirrors [`RepositoryWritePayloadV1`](crate::RepositoryWritePayloadV1)
/// family for family: the write payload is a single closed enum spanning all
/// five families even though no single executor owns every family. The
/// repository attachment executes profile/project/session and rejects
/// code/effects (which the graph shard and the writer ledger own); the read
/// contract keeps the same unified vocabulary with the same ownership split.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadOperationV1 {
    Profile(ProfileReadOperationV1),
    Project(ProjectReadOperationV1),
    Session(SessionReadOperationV1),
    Code(CodeReadOperationV1),
    Effects(EffectsReadOperationV1),
}

/// One repository read result, mirroring [`RepositoryReadOperationV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadResultV1 {
    Profile(ProfileReadResultV1),
    Project(Box<ProjectReadResultV1>),
    Session(SessionReadResultV1),
    Code(Box<CodeReadResultV1>),
    Effects(Box<EffectsReadResultV1>),
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

/// Code-family (Git index transaction) read operations.
///
/// These mirror the read surface of
/// [`GitIndexTransactionStore`](crate::GitIndexTransactionStore): a point lookup
/// of an immutable preview, a point lookup of a durable transaction record by
/// its application idempotency key, and the two recovery listings. The recovery
/// listings are keyset-paginated because a repository can accumulate an
/// unbounded number of transaction records and a profile an unbounded number of
/// repositories that need recovery.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeReadOperationV1 {
    Preview(GitIndexPreviewId),
    TransactionRecord(GitIndexIdempotencyKey),
    RecoveryCandidates(CodeRecoveryCandidatesQueryV1),
    RecoveryRepositories(CodeRecoveryRepositoriesQueryV1),
}

/// Keyset-paginated request for a repository's non-terminal recovery records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeRecoveryCandidatesQueryV1 {
    pub repository_id: RepositoryId,
    /// Exclusive lower bound; walk starts after this idempotency key.
    pub after: Option<GitIndexIdempotencyKey>,
    /// Maximum records returned. Zero yields an empty page.
    pub limit: u32,
}

/// Keyset-paginated request for the repositories that hold recovery records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeRecoveryRepositoriesQueryV1 {
    /// Exclusive lower bound; walk starts after this repository id.
    pub after: Option<RepositoryId>,
    /// Maximum repositories returned. Zero yields an empty page.
    pub limit: u32,
}

/// Code-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeReadResultV1 {
    Preview(Box<Option<GitIndexPreviewV1>>),
    TransactionRecord(Box<Option<GitIndexTransactionRecordV1>>),
    RecoveryCandidates(CodeRecoveryCandidatesPageV1),
    RecoveryRepositories(CodeRecoveryRepositoriesPageV1),
}

/// One keyset page of recovery transaction records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeRecoveryCandidatesPageV1 {
    pub records: Vec<GitIndexTransactionRecordV1>,
    /// Cursor to resume after the last returned record, or `None` at the end.
    pub next: Option<GitIndexIdempotencyKey>,
}

/// One keyset page of repositories with recovery records.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeRecoveryRepositoriesPageV1 {
    pub repositories: Vec<RepositoryId>,
    /// Cursor to resume after the last returned repository, or `None` at the end.
    pub next: Option<RepositoryId>,
}

/// Effects-family (transactional outbox/inbox) read operations.
///
/// Point lookups mirror the ledger's `outbox_entry`/inbox receipt reads; the
/// page walks are keyset-paginated because both ledger tables grow without
/// bound. Outbox pages walk `(source_sequence, effect_id)` and inbox pages walk
/// `(target_sequence, effect_id)` — the exact orderings the ledger indexes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectsReadOperationV1 {
    OutboxEntry {
        binding: StoreRuntimeBindingV1,
        effect_id: StoreEffectIdV1,
    },
    OutboxPage(EffectsOutboxPageQueryV1),
    InboxReceipt {
        binding: StoreRuntimeBindingV1,
        effect_id: StoreEffectIdV1,
    },
    InboxPage(EffectsInboxPageQueryV1),
}

/// Keyset-paginated request for a source shard's outbox entries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsOutboxPageQueryV1 {
    pub binding: StoreRuntimeBindingV1,
    /// Exclusive lower bound in `(source_sequence, effect_id)` order.
    pub after: Option<EffectsOutboxCursorV1>,
    /// Maximum entries returned. Zero yields an empty page.
    pub limit: u32,
}

/// Keyset cursor into a source shard's outbox ordering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsOutboxCursorV1 {
    pub source_sequence: u64,
    pub effect_id: StoreEffectIdV1,
}

/// Keyset-paginated request for a target shard's inbox receipts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsInboxPageQueryV1 {
    pub binding: StoreRuntimeBindingV1,
    /// Exclusive lower bound in `(target_sequence, effect_id)` order.
    pub after: Option<EffectsInboxCursorV1>,
    /// Maximum receipts returned. Zero yields an empty page.
    pub limit: u32,
}

/// Keyset cursor into a target shard's inbox ordering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsInboxCursorV1 {
    pub target_sequence: u64,
    pub effect_id: StoreEffectIdV1,
}

/// Effects-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectsReadResultV1 {
    OutboxEntry(Option<Box<TransactionalOutboxEntryV1>>),
    OutboxPage(EffectsOutboxPageV1),
    InboxReceipt(Option<Box<TransactionalInboxReceiptV1>>),
    InboxPage(EffectsInboxPageV1),
}

/// One keyset page of outbox entries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsOutboxPageV1 {
    pub entries: Vec<TransactionalOutboxEntryV1>,
    /// Cursor to resume after the last returned entry, or `None` at the end.
    pub next: Option<EffectsOutboxCursorV1>,
}

/// One keyset page of inbox receipts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectsInboxPageV1 {
    pub receipts: Vec<TransactionalInboxReceiptV1>,
    /// Cursor to resume after the last returned receipt, or `None` at the end.
    pub next: Option<EffectsInboxCursorV1>,
}
