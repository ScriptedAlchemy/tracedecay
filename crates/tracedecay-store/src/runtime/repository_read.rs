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
//! concrete runtime crate; this module owns only the contract.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalObservationIdV1, CodeGenerationId, ConfigurationRevisionId, DurableObservationV1,
    FactLineageEventV1, FileOccurrenceId, GenerationDiagnosticV1, GitIndexIdempotencyKey,
    GitIndexPreviewId, GitIndexPreviewV1, NativeAliasV2, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceIdentityV1, ProjectionGenerationId, RepositoryId,
    RetrievalAnchorId, RetrievalAnchorRecordV2, SessionId, SessionProjectionGenerationV1,
    SessionSummaryIdV1, SessionSummaryRecordV1, SourceBindingIdentityV1, SourceBindingOwnerV1,
};

use crate::{
    ConfigurationRevisionRecordV1, EvidenceAssemblyReadOperationV1, EvidenceAssemblyReadResultV1,
    FactCurrentQuery, FactLineageQuery, GitIndexTransactionRecordV1,
    RepositoryProvenanceAttachmentV1, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, RetrievalAnchorOwnerV1, RetrievalAnchorTombstoneV1,
    SessionTemporalProjectionBatchV1, SourceStoreStateV1, StorageRuntimeContractErrorV1,
    StoreEffectIdV1, StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1, StoredFactV1,
    StoredRetrievalAnchorRecordV1, TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

/// One repository read operation, dispatched across the profile, project,
/// external-source, session, code, and effects families.
///
/// This enum mirrors [`RepositoryWritePayloadV1`](crate::RepositoryWritePayloadV1)
/// family for family: the write payload is a single closed enum spanning all
/// typed families even though no single executor owns every family. The
/// repository attachment executes profile/project/session and rejects
/// code/effects (which the graph shard and the writer ledger own); the read
/// contract keeps the same unified vocabulary with the same ownership split.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadOperationV1 {
    Profile(ProfileReadOperationV1),
    Project(ProjectReadOperationV1),
    ExternalSource(ExternalSourceReadOperationV1),
    Session(SessionReadOperationV1),
    Code(CodeReadOperationV1),
    Effects(EffectsReadOperationV1),
}

impl RepositoryReadOperationV1 {
    pub(crate) fn validate_for_binding(
        &self,
        binding: &StoreRuntimeBindingV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        let valid = match self {
            Self::Profile(_) => matches!(&binding.shard_id.scope, StoreShardScopeV1::Profile),
            Self::Project(ProjectReadOperationV1::Fact(operation)) => {
                fact_owner_matches_shard(fact_read_owner(operation), &binding.shard_id)
            }
            Self::Project(ProjectReadOperationV1::Observation(operation)) => {
                observation_read_matches_shard(operation, &binding.shard_id)
            }
            Self::Project(ProjectReadOperationV1::Diagnostics(_)) => {
                matches!(&binding.shard_id.scope, StoreShardScopeV1::Project { .. })
            }
            Self::Project(ProjectReadOperationV1::EvidenceAssembly(operation)) => {
                evidence_owner_matches_shard(evidence_read_owner(operation), &binding.shard_id)
            }
            Self::ExternalSource(operation) => {
                external_source_read_matches_shard(operation, &binding.shard_id)
            }
            Self::Project(ProjectReadOperationV1::RetrievalAnchor(operation)) => {
                retrieval_owner_matches_shard(retrieval_read_owner(operation), &binding.shard_id)
            }
            Self::Session(_) => matches!(
                &binding.shard_id.scope,
                StoreShardScopeV1::ProfileSessions | StoreShardScopeV1::ProjectSessions { .. }
            ),
            Self::Code(operation) => code_read_matches_shard(operation, &binding.shard_id),
            Self::Effects(operation) => effects_read_binding(operation) == binding,
        };
        if valid {
            Ok(())
        } else {
            Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: "repository read",
                shard_family: shard_family(&binding.shard_id.scope),
            })
        }
    }
}

fn fact_read_owner(operation: &FactReadOperationV1) -> &tracedecay_domain::FactOwnerV1 {
    match operation {
        FactReadOperationV1::Current(query) => query.owner(),
        FactReadOperationV1::Lineage(query) => query.owner(),
    }
}

fn fact_owner_matches_shard(
    owner: &tracedecay_domain::FactOwnerV1,
    shard: &StoreShardIdV1,
) -> bool {
    match owner {
        tracedecay_domain::FactOwnerV1::Project { project_id } => matches!(
            &shard.scope,
            StoreShardScopeV1::Project {
                project_id: shard_project,
            } if shard_project == project_id
        ),
        tracedecay_domain::FactOwnerV1::Profile => {
            matches!(&shard.scope, StoreShardScopeV1::ProfileMemory)
        }
    }
}

fn observation_read_matches_shard(
    operation: &ObservationReadOperationV1,
    shard: &StoreShardIdV1,
) -> bool {
    match operation {
        ObservationReadOperationV1::SourceCursor { scope, .. }
        | ObservationReadOperationV1::RetrievalAnchorByAlias { scope, .. } => {
            match (scope, &shard.scope) {
                (ObservationScopeV1::Profile, StoreShardScopeV1::ProfileSessions) => true,
                (
                    ObservationScopeV1::Project { project_id },
                    StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project,
                    },
                ) => project_id == shard_project,
                _ => false,
            }
        }
        ObservationReadOperationV1::Observation { .. }
        | ObservationReadOperationV1::Replay { .. }
        | ObservationReadOperationV1::NextQueuedProjection { .. }
        | ObservationReadOperationV1::ProjectionCheckpoint
        | ObservationReadOperationV1::ProjectionRebuildProgress => matches!(
            &shard.scope,
            StoreShardScopeV1::ProfileSessions | StoreShardScopeV1::ProjectSessions { .. }
        ),
    }
}

fn evidence_read_owner(
    operation: &EvidenceAssemblyReadOperationV1,
) -> &crate::EvidenceAssemblyOwnerV1 {
    match operation {
        EvidenceAssemblyReadOperationV1::PublicationByIdempotency { owner, .. }
        | EvidenceAssemblyReadOperationV1::ContributionPage { owner, .. } => owner,
    }
}

fn evidence_owner_matches_shard(
    owner: &crate::EvidenceAssemblyOwnerV1,
    shard: &StoreShardIdV1,
) -> bool {
    owner.owner.profile_id() == &shard.profile_id
        && match (&shard.scope, owner.owner.project_id()) {
            (
                StoreShardScopeV1::Project {
                    project_id: shard_project,
                }
                | StoreShardScopeV1::ProjectSessions {
                    project_id: shard_project,
                },
                Some(project_id),
            ) => shard_project == project_id,
            (StoreShardScopeV1::ProfileSessions, None) => true,
            _ => false,
        }
}

fn external_source_read_matches_shard(
    operation: &ExternalSourceReadOperationV1,
    shard: &StoreShardIdV1,
) -> bool {
    let binding = match operation {
        ExternalSourceReadOperationV1::State { binding } => binding,
    };
    binding.validate().is_ok()
        && match (&binding.owner, &shard.scope) {
            (
                SourceBindingOwnerV1::Project(project_id),
                StoreShardScopeV1::Project {
                    project_id: shard_project,
                }
                | StoreShardScopeV1::ProjectSessions {
                    project_id: shard_project,
                },
            ) => project_id == shard_project,
            (
                SourceBindingOwnerV1::Profile(profile_id),
                StoreShardScopeV1::Profile | StoreShardScopeV1::ProfileSessions,
            ) => profile_id == &shard.profile_id,
            _ => false,
        }
}

fn retrieval_read_owner(operation: &RetrievalAnchorReadOperationV1) -> &RetrievalAnchorOwnerV1 {
    match operation {
        RetrievalAnchorReadOperationV1::AnchorById { owner, .. }
        | RetrievalAnchorReadOperationV1::CurrentDisposition { owner, .. }
        | RetrievalAnchorReadOperationV1::Derivatives { owner, .. }
        | RetrievalAnchorReadOperationV1::Tombstone { owner, .. } => owner,
    }
}

fn retrieval_owner_matches_shard(owner: &RetrievalAnchorOwnerV1, shard: &StoreShardIdV1) -> bool {
    match owner {
        RetrievalAnchorOwnerV1::V3(owner) => {
            owner.profile_id() == &shard.profile_id
                && match (&shard.scope, owner.project_id()) {
                    (
                        StoreShardScopeV1::Project {
                            project_id: shard_project,
                        }
                        | StoreShardScopeV1::ProjectSessions {
                            project_id: shard_project,
                        },
                        Some(project_id),
                    ) => shard_project == project_id,
                    (StoreShardScopeV1::ProfileSessions, None) => true,
                    _ => false,
                }
        }
        RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Project { project_id }) => {
            matches!(
                &shard.scope,
                StoreShardScopeV1::Project {
                    project_id: shard_project,
                } | StoreShardScopeV1::ProjectSessions {
                    project_id: shard_project,
                } if shard_project == project_id
            )
        }
        RetrievalAnchorOwnerV1::V2(tracedecay_domain::FactOwnerV1::Profile) => {
            matches!(&shard.scope, StoreShardScopeV1::ProfileSessions)
        }
    }
}

fn code_read_matches_shard(operation: &CodeReadOperationV1, shard: &StoreShardIdV1) -> bool {
    let StoreShardScopeV1::Code { repository_id, .. } = &shard.scope else {
        return false;
    };
    match operation {
        CodeReadOperationV1::RecoveryCandidates(query) => &query.repository_id == repository_id,
        CodeReadOperationV1::Preview(_)
        | CodeReadOperationV1::TransactionRecord(_)
        | CodeReadOperationV1::RecoveryRepositories(_) => true,
    }
}

fn effects_read_binding(operation: &EffectsReadOperationV1) -> &StoreRuntimeBindingV1 {
    match operation {
        EffectsReadOperationV1::OutboxEntry { binding, .. }
        | EffectsReadOperationV1::InboxReceipt { binding, .. } => binding,
        EffectsReadOperationV1::OutboxPage(query) => &query.binding,
        EffectsReadOperationV1::InboxPage(query) => &query.binding,
    }
}

fn shard_family(scope: &StoreShardScopeV1) -> &'static str {
    match scope {
        StoreShardScopeV1::Profile => "profile",
        StoreShardScopeV1::ProfileMemory => "profile_memory",
        StoreShardScopeV1::ProfileSessions => "profile_sessions",
        StoreShardScopeV1::Project { .. } => "project",
        StoreShardScopeV1::ProjectSessions { .. } => "project_sessions",
        StoreShardScopeV1::Code { .. } => "code",
    }
}

/// One repository read result, mirroring [`RepositoryReadOperationV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadResultV1 {
    Profile(ProfileReadResultV1),
    Project(Box<ProjectReadResultV1>),
    ExternalSource(ExternalSourceReadResultV1),
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
    EvidenceAssembly(EvidenceAssemblyReadOperationV1),
    RetrievalAnchor(RetrievalAnchorReadOperationV1),
}

/// Project-family read results across facts, observations, and diagnostics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
// Boxing the large variant is wire-transparent but would change this public
// store-protocol API and ripple through construction/match sites.
#[allow(clippy::large_enum_variant)]
pub enum ProjectReadResultV1 {
    Fact(FactReadResultV1),
    Observation(ObservationReadResultV1),
    Diagnostics(DiagnosticReadResultV1),
    EvidenceAssembly(EvidenceAssemblyReadResultV1),
    RetrievalAnchor(RetrievalAnchorReadResultV1),
}

/// Exact owner-bound external source state. The binding identity contains no
/// raw provider locator or mutable path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSourceReadOperationV1 {
    State { binding: SourceBindingIdentityV1 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSourceReadResultV1 {
    State(Option<Box<SourceStoreStateV1>>),
}

/// Retrieval-anchor authority reads. Application authorization must run before
/// any result is disclosed to a caller.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalAnchorReadOperationV1 {
    AnchorById {
        anchor_id: RetrievalAnchorId,
        owner: RetrievalAnchorOwnerV1,
    },
    CurrentDisposition {
        anchor_id: RetrievalAnchorId,
        owner: RetrievalAnchorOwnerV1,
    },
    Derivatives {
        anchor_id: RetrievalAnchorId,
        owner: RetrievalAnchorOwnerV1,
    },
    Tombstone {
        anchor_id: RetrievalAnchorId,
        owner: RetrievalAnchorOwnerV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
// Boxing the large variant is wire-transparent but would change this public
// store-protocol API and ripple through construction/match sites.
#[allow(clippy::large_enum_variant)]
pub enum RetrievalAnchorReadResultV1 {
    Anchor(Option<StoredRetrievalAnchorRecordV1>),
    CurrentDisposition(Option<RetrievalAnchorDispositionRecordV1>),
    Derivatives(Vec<RetrievalAnchorDerivativeV1>),
    Tombstone(Option<RetrievalAnchorTombstoneV1>),
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
    RetrievalAnchorByAlias {
        scope: ObservationScopeV1,
        alias: NativeAliasV2,
    },
    Replay {
        after_sequence: u64,
        limit: u16,
    },
    NextQueuedProjection {
        now_micros: i64,
    },
    ProjectionCheckpoint,
    ProjectionRebuildProgress,
}

/// One stored observation row projected with its commit sequence and cursor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredObservationRowV1 {
    pub sequence: u64,
    pub observation: DurableObservationV1,
    pub committed_cursor: ObservationSourceCursorV1,
    pub retrieval_anchor: RetrievalAnchorRecordV2,
    pub projection_generation: ProjectionGenerationId,
    pub repository_provenance: RepositoryProvenanceAttachmentV1,
    pub projection_queued: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionRebuildStateV1 {
    Aliasing,
    Building,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRebuildProgressV1 {
    pub generation: ProjectionGenerationId,
    pub frontier_sequence: u64,
    pub aliases_staged_through: u64,
    pub staged_through: u64,
    pub projected_rows: u64,
    pub skipped_observations: u64,
    pub state: ProjectionRebuildStateV1,
}

/// Observation-family read results.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationReadResultV1 {
    SourceCursor(Option<ObservationSourceCursorV1>),
    Observation(Box<Option<StoredObservationRowV1>>),
    RetrievalAnchorByAlias(Option<RetrievalAnchorId>),
    Replay(Vec<StoredObservationRowV1>),
    NextQueuedProjection(Option<CanonicalObservationIdV1>),
    ProjectionCheckpoint(u64),
    ProjectionRebuildProgress(Option<ProjectionRebuildProgressV1>),
}

/// Diagnostic-family read operations.
///
/// The variant set covers the whole read surface of
/// [`DiagnosticStore`](crate::DiagnosticStore) so a storage cutover cannot
/// silently drop a lane: `Stale` answers `stale_diagnostics` and
/// `SupersessionChain` answers `diagnostic_supersession_chain`. Both are
/// history lanes — they read records that active publication excludes — and
/// neither may re-admit a stale record into the current set.
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
    /// Superseded and cleared records bound to one generation.
    Stale(CodeGenerationId),
    /// The logical finding chain rooted at one diagnostic anchor, oldest first.
    SupersessionChain(RetrievalAnchorId),
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

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        AnchorOwnerBindingV1, BrainId, FactOwnerV1, ManifestDigest, PrivacyDomainId, ProjectId,
        RepositoryId, RetrievalAnchorId, SessionId, UserProfileId, WorktreeId,
    };

    use super::*;
    use crate::{
        CodeShardScopeV1, EvidenceAssemblyIdempotencyKeyV1, EvidenceAssemblyOwnerV1,
        StoreAuthorityEpochV1, StoreIncarnationV1,
    };

    fn binding(profile: &str, scope: StoreShardScopeV1) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::new(
                BrainId::new("brain.fixture").unwrap(),
                UserProfileId::new(profile).unwrap(),
                scope,
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    fn project(value: &str) -> ProjectId {
        ProjectId::new(value).unwrap()
    }

    fn evidence_owner(profile: &str, project_id: Option<ProjectId>) -> EvidenceAssemblyOwnerV1 {
        let profile_id = UserProfileId::new(profile).unwrap();
        let privacy = PrivacyDomainId::new("privacy.fixture").unwrap();
        EvidenceAssemblyOwnerV1 {
            owner: match project_id {
                Some(project_id) => {
                    AnchorOwnerBindingV1::for_project(profile_id, project_id, privacy).unwrap()
                }
                None => AnchorOwnerBindingV1::for_profile(profile_id, privacy).unwrap(),
            },
            scope_digest: ManifestDigest::new(format!("sha256:{}", "aa".repeat(32))).unwrap(),
            key_epoch: 1,
        }
    }

    #[test]
    fn repository_reads_require_exact_family_profile_and_project_binding() {
        let project_a = project("project.a");
        let project_b = project("project.b");
        let project_sessions_a = binding(
            "profile.a",
            StoreShardScopeV1::ProjectSessions {
                project_id: project_a.clone(),
            },
        );
        let project_sessions_b = binding(
            "profile.a",
            StoreShardScopeV1::ProjectSessions {
                project_id: project_b.clone(),
            },
        );
        let project_a_binding = binding(
            "profile.a",
            StoreShardScopeV1::Project {
                project_id: project_a.clone(),
            },
        );
        let profile_sessions_a = binding("profile.a", StoreShardScopeV1::ProfileSessions);
        let profile_sessions_b = binding("profile.b", StoreShardScopeV1::ProfileSessions);
        let profile_memory_a = binding("profile.a", StoreShardScopeV1::ProfileMemory);

        let source_cursor = RepositoryReadOperationV1::Project(
            ProjectReadOperationV1::Observation(ObservationReadOperationV1::SourceCursor {
                source: ObservationSourceIdentityV1::new(
                    SessionId::new("session.fixture").unwrap(),
                )
                .unwrap(),
                scope: ObservationScopeV1::Project {
                    project_id: project_a.clone(),
                },
            }),
        );
        assert!(
            source_cursor
                .validate_for_binding(&project_sessions_a)
                .is_ok()
        );
        assert!(
            source_cursor
                .validate_for_binding(&project_sessions_b)
                .is_err()
        );
        assert!(
            source_cursor
                .validate_for_binding(&project_a_binding)
                .is_err()
        );

        let evidence =
            RepositoryReadOperationV1::Project(ProjectReadOperationV1::EvidenceAssembly(
                EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                    owner: evidence_owner("profile.a", Some(project_a.clone())),
                    idempotency_key: EvidenceAssemblyIdempotencyKeyV1::new(
                        ManifestDigest::new(format!("sha256:{}", "bb".repeat(32))).unwrap(),
                    )
                    .unwrap(),
                },
            ));
        assert!(evidence.validate_for_binding(&project_sessions_a).is_ok());
        assert!(evidence.validate_for_binding(&project_sessions_b).is_err());
        assert!(evidence.validate_for_binding(&profile_sessions_a).is_err());

        let profile_evidence =
            RepositoryReadOperationV1::Project(ProjectReadOperationV1::EvidenceAssembly(
                EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                    owner: evidence_owner("profile.a", None),
                    idempotency_key: EvidenceAssemblyIdempotencyKeyV1::new(
                        ManifestDigest::new(format!("sha256:{}", "cc".repeat(32))).unwrap(),
                    )
                    .unwrap(),
                },
            ));
        assert!(
            profile_evidence
                .validate_for_binding(&profile_sessions_a)
                .is_ok()
        );
        assert!(
            profile_evidence
                .validate_for_binding(&profile_sessions_b)
                .is_err()
        );

        let retrieval = RepositoryReadOperationV1::Project(
            ProjectReadOperationV1::RetrievalAnchor(RetrievalAnchorReadOperationV1::AnchorById {
                anchor_id: RetrievalAnchorId::new("retrieval.fixture").unwrap(),
                owner: FactOwnerV1::Project {
                    project_id: project_a.clone(),
                }
                .into(),
            }),
        );
        assert!(retrieval.validate_for_binding(&project_a_binding).is_ok());
        assert!(retrieval.validate_for_binding(&project_sessions_a).is_ok());
        assert!(retrieval.validate_for_binding(&project_sessions_b).is_err());

        assert!(fact_owner_matches_shard(
            &FactOwnerV1::Project {
                project_id: project_a,
            },
            &project_a_binding.shard_id,
        ));
        assert!(!fact_owner_matches_shard(
            &FactOwnerV1::Project {
                project_id: project_b,
            },
            &project_a_binding.shard_id,
        ));
        assert!(!fact_owner_matches_shard(
            &FactOwnerV1::Profile,
            &profile_sessions_a.shard_id,
        ));
        assert!(fact_owner_matches_shard(
            &FactOwnerV1::Profile,
            &profile_memory_a.shard_id,
        ));
    }

    #[test]
    fn repository_code_and_effect_reads_cannot_cross_bound_runtime_identity() {
        let repository_a = RepositoryId::new("repository.a").unwrap();
        let repository_b = RepositoryId::new("repository.b").unwrap();
        let code_binding = binding(
            "profile.a",
            StoreShardScopeV1::Code {
                project_id: project("project.a"),
                repository_id: repository_a.clone(),
                scope: CodeShardScopeV1::Worktree {
                    worktree_id: WorktreeId::new("worktree.a").unwrap(),
                },
            },
        );
        let wrong_repository = RepositoryReadOperationV1::Code(
            CodeReadOperationV1::RecoveryCandidates(CodeRecoveryCandidatesQueryV1 {
                repository_id: repository_b,
                after: None,
                limit: 1,
            }),
        );
        assert!(
            wrong_repository
                .validate_for_binding(&code_binding)
                .is_err()
        );

        let profile_binding = binding("profile.a", StoreShardScopeV1::Profile);
        let mut wrong_binding = profile_binding.clone();
        wrong_binding.authority_epoch = StoreAuthorityEpochV1::new(2).unwrap();
        let effects = RepositoryReadOperationV1::Effects(EffectsReadOperationV1::OutboxEntry {
            binding: wrong_binding,
            effect_id: StoreEffectIdV1::new("effect.fixture").unwrap(),
        });
        assert!(effects.validate_for_binding(&profile_binding).is_err());
    }
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
