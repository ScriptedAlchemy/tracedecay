//! Production composition for immutable code-index generation publication.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use thiserror::Error;
use tracedecay_code_extraction::ExtractedSchemaEvidenceV1;
use tracedecay_code_extraction::incremental::{ParseDocumentIdentity, ParseError};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId,
    CodeGenerationManifestV1, CodeGenerationSourceCommitmentsV1, CodeIndexCapabilityManifestV1,
    ComponentVersion, CoverageSummaryV1, ExtractorRevision, FileOccurrenceId,
    GenerationTestAttributionV1, ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1,
    ProviderEvaluationStateV1, RefId, RelationEdgeKindV1, RepositoryId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1,
    SymbolOccurrenceId, TestAttributionEvidenceClassV1, UtcMicros, ValidatedCodeFileV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_runtime_core::resident_memory::{
    ProcessSharedMemoryReservationV1, ResidentMemoryComponentIdV1, ResidentMemoryOperationV1,
    ResidentMemoryReconstructionDemandV1,
};

use super::{
    capabilities::{
        BaseCapabilityEmitter, CapabilityEmissionErrorV1, expected_seal_digest,
        generation_language_revisions_match,
    },
    chunks::{
        ChunkingFailureV1, CodeFileIndexArtifactsV1, CodeIndexEdgeAbstentionV1,
        CodeIndexImportEvidenceV1, CodeIndexUnresolvedReferenceV1, DeterministicCodeChunker,
        ExactExtractionAuthorityV1, ExtractionAdmittedCodeSearchChunkV1, content_digest,
    },
    clones::{ClonePayloadBuildStatsV1, CodeIndexCloneBodyV1},
    extract::{ExtractionCancellation, TreeSitterExtractor, rebind_extraction_batch},
    generations::{GenerationPlanner, GenerationPlanningErrorV1, placeholder_digest},
    incremental::{ChunkIncrementErrorV1, GenerationChunkManifestV1},
    intake::{
        CodeIndexIntake, ReceiptBoundCodeFileAuthorityV1, ReceiptBoundCodeFileV1,
        SanitizedCodeIntake, SanitizedSnapshotCapabilityV1,
    },
    languages::{LanguageRegistry, StaticLanguageRegistry},
    lineage::{GenerationSymbolIndexV1, LineageResolutionErrorV1, SymbolLineageResolver},
    projection::{
        ProjectionPublicationErrorV1, ProjectionPublicationHandoffV1, expected_request_digest,
        project_for_publication,
    },
    provider::{
        GenerationProviderCoverageV1, GenerationProviderReadV1,
        GenerationTestAttributionJoinReadPort,
    },
    retained_parse::{RetainedParsePoolStats, SharedRetainedParsePool},
    test_attribution::{
        GenerationTestJoinErrorV1, GenerationTestJoinV1, TestAttributionJoinInputCoverageV1,
        TestAttributionOccurrenceV1,
    },
};

mod canonical_json;
mod clone_rows;
mod file_evidence_rows;
mod go_satisfaction;
mod helpers;
mod module_resolution;
mod projection_rows;
mod resolution_index;
mod resolution_view;
mod typescript_resolution;
pub use helpers::generation_language_revisions_are_current;
use helpers::*;
mod ignored_sources;
use ignored_sources::IgnoredSourceRosterV1;
pub use ignored_sources::{
    CodeIndexBuildRequestV1, CodeIndexIgnoredSourceAdmissionV1, CodeIndexRepositoryParseIdentityV1,
    MAX_IGNORED_DEPENDENCY_ENTRYPOINT_BYTES_V1,
};
mod import_evidence;
use import_evidence::{derive_import_evidence, validate_import_evidence};
mod parser_artifacts;
use crate::chunks::CodeSearchEligibilityV1;
use crate::extract::{ExtractionBatchV1, ExtractionFailureV1};
use crate::intake::{IntakeRejectionV1, ValidatedCodeSnapshotV1};
use crate::lineage::SymbolLineageCandidateV1;
use parser_artifacts::parse_for_indexing;
mod generation_attribution;
pub use generation_attribution::PublishedGenerationTestAttributionAuthorityV1;
mod generation_statistics;
pub use generation_statistics::CodeIndexGenerationStatisticsV1;
mod lexical_page_source;
pub use lexical_page_source::VerifiedSealedLexicalCursorRestoreErrorV1;
pub use lexical_page_source::{
    SealedLexicalFileReplacementV1, VerifiedSealedLexicalCursorV1,
    VerifiedSealedLexicalPageBatchBoundsV1, VerifiedSealedLexicalPageBatchReadV1,
    VerifiedSealedLexicalPageReadV1, VerifiedSealedLexicalPageSourceV1,
    VerifiedSealedLexicalPageV1, VerifiedSealedLexicalSourceReceiptV1,
    VerifiedSealedLexicalSymbolDisplayV1, VerifiedSealedTextGenerationMetadataV1,
    advance_import_dictionary_digest, initial_import_dictionary_digest,
};
mod decoded_content;
pub use decoded_content::{DecodedGenerationContentV1, SharedDecodedContentPoolV1};
mod graph_build_bound;
pub use graph_build_bound::CodeGraphBuildBoundV1;
pub(crate) use graph_build_bound::{layered_page_graph_build_bound, sealed_page_graph_build_bound};
mod graph_page_store;
mod graph_pages;
mod memory_store;
pub use memory_store::{MemoryCodeIndexPreparationV1, MemorySealedPublicationStoreV1};
mod sealed_parent;
use sealed_parent::SealedParentGenerationV1;
pub use sealed_parent::{CodeIndexSealedGenerationV1, SealedSegmentReaderV1};
mod sparse_increment;
pub use sparse_increment::CodeIndexColdBuildReasonV1;
use sparse_increment::SparseBuildV1;
mod sparse_resolution;
mod sparse_successor;
#[cfg(test)]
pub(crate) use graph_page_store::CodeGraphPageBuildFootprintV1;
pub(crate) use graph_page_store::{
    CodeGraphPageDescriptorV1, CodeGraphPageStoreV1, CodeGraphPageStoreWriterV1,
    FileCodeGraphPageStoreV1, SealedCodeGraphPageStoreV1,
};
pub(crate) use graph_pages::PersistedCodeGraphPageV1;
mod partitioned_codec;
pub(crate) mod resident_bytes;
mod resolution_outputs;
pub use partitioned_codec::{
    SealedGenerationFileWindowsV1, SealedGenerationSegmentIdentityV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
    SealedGenerationSegmentReaderV1,
};
mod sealed_codec;
pub use sealed_codec::{
    MAX_SEALED_CODE_GENERATION_BYTES_V1, SEALED_GENERATION_FORMAT_REVISION_V1,
    superseded_sealed_generation_revision,
};

/// Current daemon chunker identity shared by production indexing and native
/// semantic evaluation fixtures. Historical revisions remain decodable but
/// must never be emitted as current activation evidence.
///
/// `v5` resolves constructor and associated owner uses to type definitions
/// and preserves each parser-observed call token as its relation evidence.
pub const DAEMON_CODE_INDEX_CHUNKER_REVISION: &str = "chunker.daemon.v5";

/// Immutable configuration retained by one production index owner.
#[derive(Clone, Debug)]
pub struct CodeIndexProductionConfigV1 {
    pub project_id: ProjectId,
    pub repository: RepositoryId,
    pub sanitizer_revision: SanitizerRevision,
    pub policy_revision: PolicyRevisionId,
    pub chunker_revision: tracedecay_domain::ChunkerRevision,
    pub privacy_domain: PrivacyDomainId,
    pub privacy_key_epoch: u64,
    /// When set, intake rejects source snapshots older than this bound.
    pub max_snapshot_age_micros: Option<i64>,
}

impl CodeIndexProductionConfigV1 {
    fn validate(&self) -> Result<(), CodeIndexProductionOpenErrorV1> {
        if self.project_id.validate().is_err()
            || self.repository.validate().is_err()
            || self.sanitizer_revision.validate().is_err()
            || self.policy_revision.validate().is_err()
            || self.chunker_revision.validate().is_err()
            || self.privacy_domain.validate().is_err()
        {
            return Err(CodeIndexProductionOpenErrorV1::InvalidConfiguration);
        }
        if self.max_snapshot_age_micros.is_some_and(|age| age < 0) {
            return Err(CodeIndexProductionOpenErrorV1::InvalidSnapshotAge);
        }
        Ok(())
    }
}

/// One owner input that prevents a sealed generation from being reused.
///
/// These reason codes are stable status vocabulary. They deliberately identify
/// the incompatible authority rather than embedding current or prior values,
/// which may include private project configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CodeIndexGenerationIncompatibilityV1 {
    Project,
    LanguageRevisions,
    SanitizerRevision,
    PolicyRevision,
    MixedPolicyRevisions,
    ChunkerRevision,
    PrivacyDomain,
    PrivacyKeyEpoch,
}

impl CodeIndexGenerationIncompatibilityV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::LanguageRevisions => "language_revisions",
            Self::SanitizerRevision => "sanitizer_revision",
            Self::PolicyRevision => "policy_revision",
            Self::MixedPolicyRevisions => "mixed_policy_revisions",
            Self::ChunkerRevision => "chunker_revision",
            Self::PrivacyDomain => "privacy_domain",
            Self::PrivacyKeyEpoch => "privacy_key_epoch",
        }
    }
}

/// Compatibility witness between one immutable generation and one production
/// owner configuration.
///
/// Every incompatibility retires the generation from incremental reuse. A
/// chunker-only mismatch may keep serving while its replacement builds because
/// its already-sealed bytes still satisfy the same project, sanitizer, policy,
/// and privacy authorities. Every other mismatch is a fail-closed serving
/// refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexGenerationCompatibilityV1 {
    incompatibilities: BTreeSet<CodeIndexGenerationIncompatibilityV1>,
}

impl CodeIndexGenerationCompatibilityV1 {
    fn for_metadata(
        manifest: &CodeGenerationManifestV1,
        snapshot: &SanitizedCodeSnapshotV1,
        config: &CodeIndexProductionConfigV1,
    ) -> Self {
        let mut incompatibilities = BTreeSet::new();
        if manifest.project_id != config.project_id {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::Project);
        }
        if !generation_language_revisions_are_current(manifest, snapshot) {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::LanguageRevisions);
        }
        if manifest.sanitizer_revision != config.sanitizer_revision {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::SanitizerRevision);
        }
        if manifest.chunker_revision != config.chunker_revision {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::ChunkerRevision);
        }
        if manifest.privacy_domain != config.privacy_domain {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::PrivacyDomain);
        }
        if manifest.privacy_key_epoch != config.privacy_key_epoch {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::PrivacyKeyEpoch);
        }
        Self { incompatibilities }
    }

    pub fn incompatibilities(&self) -> &BTreeSet<CodeIndexGenerationIncompatibilityV1> {
        &self.incompatibilities
    }

    pub fn is_reusable(&self) -> bool {
        self.incompatibilities.is_empty()
    }

    pub fn may_serve_while_rebuilding(&self) -> bool {
        self.incompatibilities.iter().all(|reason| {
            matches!(
                reason,
                CodeIndexGenerationIncompatibilityV1::ChunkerRevision
            )
        })
    }
}

/// One sanitized byte payload paired with immutable snapshot metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexCapturedFileV1 {
    pub file_occurrence_id: FileOccurrenceId,
    /// Canonical sanitized source allocation retained by the snapshot while
    /// production reads it. Domain intake materializes only bounded per-file
    /// `Vec` inputs where its serializable contract requires ownership.
    pub sanitized_bytes: Arc<[u8]>,
    pub sensitivity_level: SensitivityLevelV1,
}

/// Synchronous checkpoints exposed by an application/daemon request.
pub trait CodeIndexExecutionControlV1: Sync {
    fn is_cancelled(&self) -> bool;
    fn is_deadline_exceeded(&self) -> bool;
}

/// A control that never interrupts. Sealed seating uses it when the caller
/// already proved the envelope is admissible and only wants the streaming
/// restore, not a request-scoped cancel/deadline.
pub struct UninterruptibleCodeIndexControlV1;

impl CodeIndexExecutionControlV1 for UninterruptibleCodeIndexControlV1 {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// The terminal reason an index run abstained before publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexInterruptionV1 {
    Cancelled,
    DeadlineExceeded,
}

struct ExtractionControlBridge<'a> {
    control: &'a dyn CodeIndexExecutionControlV1,
}

impl ExtractionCancellation for ExtractionControlBridge<'_> {
    fn is_cancelled(&self) -> bool {
        self.control.is_cancelled() || self.control.is_deadline_exceeded()
    }
}

/// Failure returned by the durable publication authority.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodeIndexPublicationStoreErrorV1 {
    #[error("the active generation changed before atomic publication")]
    CompareAndSwap,
    #[error("the publication authority is corrupt and requires an index reset: {0}")]
    CorruptionResetRequired(String),
    #[error("the publication authority is unavailable: {0}")]
    Unavailable(String),
    /// Another owner holds the code-generation store lock this operation
    /// needs, exclusive or shared. The holder releases it on its own and emits
    /// no wake. The refused pass waits for that release instead of retrying
    /// while the lock is still held.
    #[error("the code-generation store lock is held by another owner")]
    StoreLockContended,
    /// Materializing the whole generation does not fit the process
    /// resident-memory budget now; it succeeds once memory is given back.
    #[error("decoding the generation does not fit the resident-memory budget: {0}")]
    ResidentMemoryRefused(String),
}

/// Canonical active-generation slot inside one repository-owned code-index
/// store. Paths are deliberately absent: linked worktrees share the repository
/// store while their branch/worktree generations remain independently active.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexGenerationScopeV1 {
    pub repository: RepositoryId,
    pub reference: Option<RefId>,
    pub worktree: Option<WorktreeId>,
}

impl CodeIndexGenerationScopeV1 {
    pub fn for_snapshot(snapshot: &SanitizedCodeSnapshotV1) -> Self {
        Self {
            repository: snapshot.repository.clone(),
            reference: snapshot.reference.clone(),
            worktree: snapshot.worktree.clone(),
        }
    }

    /// Whether two scopes name the same physical checkout.
    ///
    /// Repository and worktree are checkout identity: a generation sealed
    /// under either of them differing belongs to another checkout and may
    /// never be adopted or served for this one. `reference` is deliberately
    /// excluded, it is the branch label HEAD happens to carry, and it moves
    /// under a fixed worktree on every ordinary commit, branch switch, or
    /// rebase, so serving gates that need only checkout identity keep
    /// admitting the checkout's own generations across a label move. Slot
    /// dispatch is stricter: [`CodeIndexProductionOwnerV1::active_generation`]
    /// demands the complete scope, label included, because branch and worktree
    /// generations stay independently active inside one shared repository
    /// store.
    #[must_use]
    pub fn identifies_same_checkout(&self, other: &Self) -> bool {
        self.repository == other.repository && self.worktree == other.worktree
    }

    /// Validate every component before a publication store binds this slot.
    pub fn validate(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.repository
            .validate()
            .and_then(|()| self.reference.as_ref().map_or(Ok(()), RefId::validate))
            .and_then(|()| self.worktree.as_ref().map_or(Ok(()), WorktreeId::validate))
            .map_err(|error| CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string()))
    }
}

/// Renders one scope for slot-dispatch refusals. An absent reference or
/// worktree is a truthful non-git/unbound component, spelled out so operators
/// can tell a misclassified checkout from a mispartitioned store.
fn describe_scope(scope: &CodeIndexGenerationScopeV1) -> String {
    format!(
        "repository {}, reference {}, worktree {}",
        scope.repository.as_str(),
        scope
            .reference
            .as_ref()
            .map_or("(none)", |reference| reference.as_str()),
        scope
            .worktree
            .as_ref()
            .map_or("(none)", |worktree| worktree.as_str()),
    )
}

/// The only persistence seam for this production owner. Implementations retain
/// one physical store per canonical repository and partition only active
/// generation pointers by [`CodeIndexGenerationScopeV1`]. They must make the
/// complete generation and verified projection receipt visible as one scoped
/// compare-and-swap operation and return the same immutable value on restart.
pub trait CodeIndexAtomicPublicationPort {
    type Preparation: CodeIndexPreparationPort + Send + 'static;

    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1>;

    /// Stage an unpublished successor without excluding active readers. The
    /// returned owner binds this physical store, scope, and exact incumbent.
    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1>;

    /// Recheck source and incumbent fences, make staged segments durable, and
    /// activate the exact prepared manifest in one publication transaction.
    /// A retryable failure leaves the preparation owned by the caller.
    fn commit(
        &mut self,
        preparation: &mut Self::Preparation,
        publication: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1>;
}

/// The canonical store owns unpublished segment bytes until commit or abort.
/// Readers can resolve a staged segment before its final durable name exists;
/// abort invalidates those readers and removes only this owner's temporaries.
pub trait CodeIndexPreparationPort {
    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1>;

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1>;

    fn abort(self) -> Result<(), CodeIndexPublicationStoreErrorV1>;
}

#[derive(Debug)]
struct ConstructionMemoryV1 {
    operation: ResidentMemoryOperationV1,
    component: ResidentMemoryComponentIdV1,
    charge: Option<ProcessSharedMemoryReservationV1>,
    charged_bytes: u64,
}

impl ConstructionMemoryV1 {
    fn new(operation: ResidentMemoryOperationV1) -> Result<Self, CodeIndexProductionErrorV1> {
        let component = ResidentMemoryComponentIdV1::new("code_index.construction.v1")
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        Ok(Self {
            operation,
            component,
            charge: None,
            charged_bytes: 0,
        })
    }

    fn resize(&mut self, bytes: u64) -> Result<(), CodeIndexProductionErrorV1> {
        if bytes > self.charged_bytes {
            let additional = NonZeroU64::new(bytes - self.charged_bytes).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("construction charge overflowed".to_owned())
            })?;
            let retained_bytes = self.charged_bytes;
            let refused = |error: tracedecay_runtime_core::resident_memory::ResidentMemoryAdmissionFailureV1| {
                tracing::trace!(prior_charged_bytes = retained_bytes, attempted_bytes = bytes,
                    additional_bytes = additional.get(), %error,
                    "construction admission refused");
                CodeIndexProductionErrorV1::Publication(
                    CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(error.to_string()),
                )
            };
            match self.charge.as_mut() {
                Some(charge) => charge.try_grow(additional).map_err(refused)?,
                None => {
                    self.charge = Some(
                        self.operation
                            .reserve_process_shared(self.component, additional)
                            .map_err(refused)?,
                    )
                }
            }
        } else if bytes == 0 {
            drop(self.charge.take());
        } else if let Some(charge) = self.charge.as_mut() {
            charge
                .shrink_to(bytes)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        } else {
            return Err(CodeIndexProductionErrorV1::Contract(
                "construction charge ownership is inconsistent".to_owned(),
            ));
        }
        self.charged_bytes = bytes;
        Ok(())
    }
}

/// A canonical hash keeps its per-thread scratch until explicitly released.
/// Pair its real lifetime with the construction reservation on every exit.
struct ConstructionCanonicalScratchV1;

impl Drop for ConstructionCanonicalScratchV1 {
    fn drop(&mut self) {
        tracedecay_domain::research::release_thread_canonical_scratch();
    }
}

impl ConstructionMemoryV1 {
    fn canonical<T>(
        &mut self,
        base: usize,
        objects: usize,
        work: impl FnOnce() -> Result<T, CodeIndexProductionErrorV1>,
    ) -> Result<T, CodeIndexProductionErrorV1> {
        tracedecay_domain::research::release_thread_canonical_scratch();
        // Canonical SHA's 64 KiB sink and the eight-slot object-buffer pool
        // coexist with the known DTO ObjectWriter allocations below.
        let pool = 8_usize.saturating_mul(std::mem::size_of::<(
            String,
            Vec<(std::borrow::Cow<'static, str>, std::ops::Range<usize>)>,
        )>());
        self.resize(
            base.saturating_add(objects)
                .saturating_add(64 * 1024)
                .saturating_add(pool) as u64,
        )?;
        let _scratch = ConstructionCanonicalScratchV1;
        work()
    }
}

fn changed_set_hash_workspace(
    changes: &ChangedCodeChunkSetV1,
) -> Result<usize, CodeIndexProductionErrorV1> {
    let row = changes
        .added_or_changed
        .iter()
        .chain(&changes.deleted)
        .try_fold(0_usize, |largest, row| {
            partitioned_codec::encoded_json_rows_length(row).map(|bytes| largest.max(bytes))
        })?;
    // The changed-set and both digest DTOs stream their checked, ordered
    // fields. Only one changed-row object is buffered at a time.
    let domains = partitioned_codec::encoded_json_rows_length(&(
        "tracedecay.changed-code-chunk-set.v1",
        "tracedecay.code-source-parent-delta.v1",
        &changes.from_generation,
    ))?;
    Ok(
        partitioned_codec::canonical_object_workspace_bytes(domains, 8)
            .saturating_add(partitioned_codec::canonical_object_workspace_bytes(row, 3))
            .saturating_add(resident_bytes::btree_bytes(
                changes
                    .added_or_changed
                    .len()
                    .saturating_add(changes.deleted.len()),
                std::mem::size_of::<&tracedecay_domain::CodeSearchChunkId>(),
            )),
    )
}

fn manifest_hash_workspace(
    manifest: &CodeGenerationManifestV1,
) -> Result<usize, CodeIndexProductionErrorV1> {
    Ok(partitioned_codec::canonical_object_workspace_bytes(
        partitioned_codec::encoded_json_rows_length(manifest)?,
        16,
    )
    .saturating_add(partitioned_codec::canonical_object_workspace_bytes(
        partitioned_codec::encoded_json_rows_length(&manifest.source_commitments)?,
        3,
    )))
}

fn metadata_hash_workspace(
    manifest: &CodeGenerationManifestV1,
    snapshot: &SanitizedCodeSnapshotV1,
) -> Result<usize, CodeIndexProductionErrorV1> {
    let snapshot_bytes = partitioned_codec::encoded_json_rows_length(snapshot)?;
    let mut file = 0_usize;
    for row in &snapshot.files {
        file = file.max(partitioned_codec::encoded_json_rows_length(row)?);
    }
    let mut omission = 0_usize;
    for row in &snapshot.omitted_sources {
        omission = omission.max(partitioned_codec::encoded_json_rows_length(row)?);
    }
    Ok(
        partitioned_codec::canonical_object_workspace_bytes(snapshot_bytes, 10)
            .saturating_add(partitioned_codec::canonical_object_workspace_bytes(file, 6))
            // An omission has a nested reason object in addition to its own row.
            .saturating_add(
                partitioned_codec::canonical_object_workspace_bytes(omission, 2).saturating_mul(2),
            )
            .saturating_add(manifest_hash_workspace(manifest)?),
    )
}

fn emit_cold_capability(
    memory: &mut ConstructionMemoryV1,
    base: usize,
    intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
    manifest: &CodeGenerationManifestV1,
    snapshot: &SanitizedCodeSnapshotV1,
    coverage: CoverageSummaryV1,
) -> Result<CodeIndexCapabilityManifestV1, CodeIndexProductionErrorV1> {
    // Intake already owns the exact snapshot-scoped registry. Rebuilding the
    // complete compiled registry here would allocate unrelated descriptors.
    let handles = manifest
        .grammar_revisions
        .len()
        .saturating_mul(std::mem::size_of::<&tracedecay_domain::LanguageDescriptorV1>());
    memory.resize(base.saturating_add(handles) as u64)?;
    let descriptors = intake.registry().descriptors();
    let workspace =
        resident_bytes::capability_construction_workspace(&descriptors, snapshot, manifest);
    let length = partitioned_codec::encoded_json_rows_length(&(
        manifest,
        &descriptors,
        &snapshot.sanitization_receipts,
        crate::capabilities::BASE_EXACT_TERM_KINDS,
        crate::capabilities::BASE_EDGE_AUTHORITY_CLASSES,
        tracedecay_domain::CodeSearchChunkGrainV1::ORDER,
    ))?;
    let descriptor_json = descriptors
        .iter()
        .try_fold(0_usize, |largest, descriptor| {
            partitioned_codec::encoded_json_rows_length(descriptor).map(|bytes| largest.max(bytes))
        })?;
    let hash_workspace = partitioned_codec::canonical_object_workspace_bytes(length, 13)
        .saturating_add(partitioned_codec::canonical_object_workspace_bytes(
            descriptor_json,
            12,
        ))
        .saturating_add(partitioned_codec::canonical_object_workspace_bytes(
            partitioned_codec::encoded_json_rows_length(&coverage)?,
            8,
        ));
    memory.canonical(
        base.saturating_add(handles).saturating_add(workspace),
        hash_workspace,
        || {
            let registry = StaticLanguageRegistry::try_from_descriptors(
                descriptors
                    .iter()
                    .map(|descriptor| (**descriptor).clone())
                    .collect(),
            )
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            BaseCapabilityEmitter::new(registry, coverage, snapshot.sanitization_receipts.clone())
                .emit(manifest)
                .map_err(CodeIndexProductionErrorV1::Capability)
        },
    )
}

/// Compact, verified output of construction. File and evidence payloads belong
/// to the store preparation rather than a decoded generation kept until CAS.
#[derive(Clone, Debug)]
pub struct CodeIndexSealedPublicationV1 {
    metadata: Arc<VerifiedSealedTextGenerationMetadataV1>,
    manifest_bytes: Arc<[u8]>,
    lane_digest: ManifestDigest,
    chunk_count: u64,
    projection: ProjectionPublicationHandoffV1,
    cold_reason: Option<CodeIndexColdBuildReasonV1>,
    clone_payloads_reused: u64,
    clone_payloads_computed: u64,
    clone_stale_invalidations: u64,
    construction_memory: Option<Arc<ConstructionMemoryV1>>,
}

impl CodeIndexSealedPublicationV1 {
    pub(super) fn from_prepared(
        manifest_bytes: Arc<[u8]>,
        projection: ProjectionPublicationHandoffV1,
        cold_reason: Option<CodeIndexColdBuildReasonV1>,
        clone_stats: (u64, u64, u64),
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let (metadata, lane_digest, chunk_count) =
            CodeIndexPublishedGenerationV1::partitioned_metadata_and_lane(&manifest_bytes)?;
        let request = projection.request();
        let expected_chunks = u64::try_from(request.changes.added_or_changed.len())
            .ok()
            .and_then(|changed| changed.checked_add(request.changes.reused_count));
        if request.changes.to_generation != metadata.manifest().generation_id
            || request.changes.manifest_digest
                != metadata.source_commitments()?.incremental_manifest_digest
            || expected_chunks != Some(chunk_count)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "prepared publication does not match its verified projection".to_owned(),
            ));
        }
        Ok(Self {
            metadata: Arc::new(metadata),
            manifest_bytes,
            lane_digest,
            chunk_count,
            projection,
            cold_reason,
            clone_payloads_reused: clone_stats.0,
            clone_payloads_computed: clone_stats.1,
            clone_stale_invalidations: clone_stats.2,
            construction_memory: None,
        })
    }

    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        self.metadata.manifest()
    }
    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        self.metadata.snapshot()
    }
    pub fn projection(&self) -> &ProjectionPublicationHandoffV1 {
        &self.projection
    }
    pub fn statistics(&self) -> &CodeIndexGenerationStatisticsV1 {
        self.metadata.generation_statistics()
    }
    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }
    pub fn manifest_bytes(&self) -> &Arc<[u8]> {
        &self.manifest_bytes
    }
    pub fn clone_update_statistics(&self) -> (u64, u64, bool) {
        (
            self.clone_payloads_reused,
            self.clone_stale_invalidations,
            self.clone_payloads_computed > 0 || self.clone_stale_invalidations > 0,
        )
    }
    pub fn cold_reason(&self) -> Option<CodeIndexColdBuildReasonV1> {
        self.cold_reason
    }
}

/// What one build published: the sealed manifest, its authenticated
/// metadata, and how the build ran.
#[derive(Clone, Debug)]
pub struct CodeIndexPublishedBuildV1 {
    publication: CodeIndexSealedPublicationV1,
    metadata: Arc<VerifiedSealedTextGenerationMetadataV1>,
    manifest_bytes: Arc<[u8]>,
    lane_digest: ManifestDigest,
    sealed: CodeIndexSealedGenerationV1,
}

impl CodeIndexPublishedBuildV1 {
    /// Describe the generation a publication store sealed from
    /// `publication` as `manifest_bytes`.
    pub fn new(
        publication: CodeIndexSealedPublicationV1,
        sealed: CodeIndexSealedGenerationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let manifest_bytes: Arc<[u8]> = Arc::from(sealed.manifest_bytes());
        if publication.manifest_bytes.as_ref() != manifest_bytes.as_ref() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "the publication store sealed a different generation".to_owned(),
            ));
        }
        Ok(Self {
            metadata: Arc::clone(&publication.metadata),
            manifest_bytes,
            lane_digest: publication.lane_digest.clone(),
            publication,
            sealed,
        })
    }

    pub fn sealed_generation(&self) -> &CodeIndexSealedGenerationV1 {
        &self.sealed
    }

    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        self.publication.manifest()
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        self.publication.snapshot()
    }

    pub fn projection(&self) -> &ProjectionPublicationHandoffV1 {
        self.publication.projection()
    }

    pub fn metadata(&self) -> &Arc<VerifiedSealedTextGenerationMetadataV1> {
        &self.metadata
    }

    pub fn manifest_bytes(&self) -> &Arc<[u8]> {
        &self.manifest_bytes
    }

    /// The digest of the published content identity, file segments, and
    /// graph pages; cold and sparse builds of one tree agree on it.
    pub fn lane_digest(&self) -> &ManifestDigest {
        &self.lane_digest
    }

    pub fn publication(&self) -> &CodeIndexSealedPublicationV1 {
        &self.publication
    }

    pub fn clone_update_statistics(&self) -> (u64, u64, bool) {
        self.publication.clone_update_statistics()
    }

    /// Why the build ran whole; `None` for a sparse successor.
    pub fn cold_reason(&self) -> Option<CodeIndexColdBuildReasonV1> {
        self.publication.cold_reason()
    }
}

#[derive(Clone, Debug)]
struct FileGenerationArtifactsV1 {
    authority: ReceiptBoundCodeFileAuthorityV1,
    extraction: ExtractionBatchV1,
    artifacts: CodeFileIndexArtifactsV1,
    exact_authority: ExactExtractionAuthorityV1,
}

impl AsRef<FileGenerationArtifactsV1> for FileGenerationArtifactsV1 {
    fn as_ref(&self) -> &FileGenerationArtifactsV1 {
        self
    }
}

const PHYSICAL_CODE_ARTIFACT_REUSE_DIGEST_DOMAIN: &str =
    "tracedecay.physical-code-artifact-reuse.v1";
const MAX_PHYSICAL_CODE_ARTIFACTS: usize = 1_024;

/// Fan `operation` across every file at once on the reserved-width indexing
/// pool (see [`crate::parallelism`]), preserving input order in the output.
///
/// There is no batch barrier: a batched fan-out re-synchronized every
/// `workers` files, so one slow file stalled a whole batch and the pipeline
/// never reached machine width. Files are independent, so the whole slice is
/// one parallel map.
///
/// Failure semantics are the sequential ones: the returned error is always
/// the lowest-index failure, independent of completion order. Unlike the
/// batched form this does not abandon later files after a failure, the
/// tradeoff for having no barrier. Cancellation still short-circuits, because
/// every per-file closure checkpoints the execution control first and
/// returns immediately once the reconcile is cancelled.
///
/// Per-unit work parses arbitrary user source, so a panic in one unit is
/// contained here and converted into that unit's typed
/// [`crate::parallelism::CodeIndexParallelismErrorV1::WorkerPanic`]. Letting
/// it unwind out of the pool instead aborted the whole fan-out and surfaced in
/// the daemon only as an opaque `JoinError`, so a single malformed file took
/// down every other file's work in the same generation.
fn collect_bounded_ordered<T, R, E, F>(items: &[T], operation: F) -> Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: From<crate::parallelism::CodeIndexParallelismErrorV1> + Send,
    F: Fn(&T) -> Result<R, E> + Sync,
{
    // Always enter the indexing pool, even when the width is 1. File-level
    // leaves are the pool actors. Chunk sweeps stay on the calling leaf so
    // they do not become a second admission class.
    crate::parallelism::install(|| {
        let run = |(index, item): (usize, &T)| -> Result<R, E> {
            crate::parallelism::with_background_cpu_permit(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(item)))
                    .unwrap_or_else(|payload| {
                        Err(E::from(
                            crate::parallelism::CodeIndexParallelismErrorV1::from_panic_payload(
                                index, &*payload,
                            ),
                        ))
                    })
            })
        };
        if items.len() < 2 || crate::parallelism::indexing_workers() < 2 {
            return items.iter().enumerate().map(&run).collect();
        }
        // Collecting every unit's result before short-circuiting keeps the
        // reported failure the lowest-index one, panic or not.
        let results: Vec<Result<R, E>> = items
            .par_iter()
            .enumerate()
            .map(&run)
            .collect::<Vec<Result<R, E>>>();
        results.into_iter().collect()
    })
    .map_err(E::from)?
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhysicalCodeArtifactPoolStatsV1 {
    pub inserted: u64,
    pub reused: u64,
    pub clone_payloads_reused: u64,
    pub clone_payloads_computed: u64,
    /// Artifact allocations still owned by a published or staged generation.
    /// The physical pool indexes these allocations weakly and never extends
    /// their lifetime.
    pub resident: u64,
}

#[derive(Default)]
struct PhysicalCodeArtifactPoolStateV1 {
    artifacts: BTreeMap<ManifestDigest, Weak<FileGenerationArtifactsV1>>,
    insertion_order: VecDeque<ManifestDigest>,
    inserted: u64,
    reused: u64,
    clone_payloads_reused: u64,
    clone_payloads_computed: u64,
}

/// Registry-scoped physical parse/chunk artifact pool. The key binds every
/// input that can change extraction or chunking; generation-local artifacts
/// are rematerialized before they leave the pool.
#[derive(Clone, Default)]
pub struct SharedPhysicalCodeArtifactPoolV1 {
    state: Arc<Mutex<PhysicalCodeArtifactPoolStateV1>>,
}

fn upgrade_weak_under_lock<S, T>(
    state: &Mutex<S>,
    select: impl FnOnce(&S) -> Option<Weak<T>>,
) -> Option<Arc<T>> {
    let state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    select(&state).and_then(|value| value.upgrade())
}

impl SharedPhysicalCodeArtifactPoolV1 {
    fn reuse(
        &self,
        key: &ManifestDigest,
        file: &ReceiptBoundCodeFileV1,
        extractor_revision: &ExtractorRevision,
    ) -> Option<Arc<FileGenerationArtifactsV1>> {
        crate::observe::measure_hot_loop!("code_index.artifact_pool.reuse", {
            let artifact =
                upgrade_weak_under_lock(&self.state, |state| state.artifacts.get(key).cloned())?;
            let rebound = Arc::new(
                artifact
                    .rematerialize_for_file(file, extractor_revision)
                    .ok()?,
            );
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.reused = state.reused.saturating_add(1);
            }
            Some(rebound)
        })
    }

    /// Record one generation-owned artifact under its physical reuse key.
    /// The pool retains only a weak index entry, so indexing a cold generation
    /// never deep-clones or pins the parsed/chunked payload.
    fn insert(&self, key: ManifestDigest, artifact: &Arc<FileGenerationArtifactsV1>) {
        crate::observe::measure_hot_loop!("code_index.artifact_pool.insert", {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(retained) = state.artifacts.get_mut(&key) {
                *retained = Arc::downgrade(artifact);
                return;
            }
            while state.artifacts.len() >= MAX_PHYSICAL_CODE_ARTIFACTS {
                let Some(evicted) = state.insertion_order.pop_front() else {
                    break;
                };
                state.artifacts.remove(&evicted);
            }
            state.insertion_order.push_back(key.clone());
            state.artifacts.insert(key, Arc::downgrade(artifact));
            state.inserted = state.inserted.saturating_add(1);
        })
    }

    /// Drop the index entries whose artifact no generation owns any more. A
    /// `Weak` keeps its allocation, so a dead entry still pins the artifact's
    /// header, and it can never be reused.
    pub fn release_dead_entries(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .artifacts
            .retain(|_, artifact| artifact.strong_count() > 0);
        let PhysicalCodeArtifactPoolStateV1 {
            artifacts,
            insertion_order,
            ..
        } = &mut *state;
        insertion_order.retain(|key| artifacts.contains_key(key));
    }

    fn record_clone_payloads(&self, reused: u64, computed: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.clone_payloads_reused = state.clone_payloads_reused.saturating_add(reused);
        state.clone_payloads_computed = state.clone_payloads_computed.saturating_add(computed);
    }

    pub fn stats(&self) -> PhysicalCodeArtifactPoolStatsV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PhysicalCodeArtifactPoolStatsV1 {
            inserted: state.inserted,
            reused: state.reused,
            clone_payloads_reused: state.clone_payloads_reused,
            clone_payloads_computed: state.clone_payloads_computed,
            resident: u64::try_from(
                state
                    .artifacts
                    .values()
                    .filter(|artifact| artifact.strong_count() > 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
        }
    }
}

impl FileGenerationArtifactsV1 {
    fn stale_clone_bindings(&self, current: &Self) -> u64 {
        let current = current
            .artifacts
            .clone_bodies
            .iter()
            .map(|body| {
                (
                    (
                        body.occurrence.path.as_str(),
                        body.occurrence.body_span.start_byte,
                        body.occurrence.body_span.end_byte,
                        body.payload.symbol_kind.as_str(),
                    ),
                    &body.payload.payload_digest,
                )
            })
            .collect::<BTreeMap<_, _>>();
        u64::try_from(
            self.artifacts
                .clone_bodies
                .iter()
                .filter(|body| {
                    current.get(&(
                        body.occurrence.path.as_str(),
                        body.occurrence.body_span.start_byte,
                        body.occurrence.body_span.end_byte,
                        body.payload.symbol_kind.as_str(),
                    )) != Some(&&body.payload.payload_digest)
                })
                .count(),
        )
        .unwrap_or(u64::MAX)
    }

    fn rematerialize_for_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
        extractor_revision: &ExtractorRevision,
    ) -> Result<Self, ChunkingFailureV1> {
        crate::observe::measure_hot_loop!("code_index.artifact_pool.rematerialize", {
            let target = file.validated_file();
            if &self.extraction.extractor_revision != extractor_revision {
                return Err(ChunkingFailureV1::GenerationMismatch);
            }
            let mut artifacts = self
                .artifacts
                .rematerialize_for_generation_reusing_clone_payloads(
                    target.generation_id.clone(),
                    target.file.file_occurrence_id.clone(),
                )?;
            for body in &mut artifacts.clone_bodies {
                body.occurrence.project_id = file.authority().project_id.clone();
                body.occurrence.repository_id = file.authority().repository_id.clone();
                body.occurrence.worktree_id = None;
                body.occurrence.snapshot_digest = target.snapshot_digest.clone();
                body.occurrence.path = file.authority().logical_path.clone();
            }
            let exact_authority = self
                .exact_authority
                .rematerialize_for_generation(&self.artifacts.chunks, &artifacts.chunks)?;
            let extraction = rebind_extraction_batch(&self.authority, &self.extraction, file)
                .map_err(|_| ChunkingFailureV1::GenerationMismatch)?;
            Ok(Self {
                authority: file.authority().clone(),
                extraction,
                artifacts,
                exact_authority,
            })
        })
    }
}

/// The complete, immutable output of one production index generation.
///
/// All fields are private so callers can inspect evidence but cannot assemble
/// a generation that bypasses intake, parser-backed exact admission, receipt
/// verification, or atomic publication.
#[derive(Clone, Debug)]
pub struct CodeIndexPublishedGenerationV1 {
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    ignored_source_roster: IgnoredSourceRosterV1,
    files: Vec<Arc<FileGenerationArtifactsV1>>,
    /// The decoded pages `files` references, shared with every generation
    /// that sealed the same content. A generation built in this process
    /// owns its pages outright and has none.
    content: Option<Arc<DecodedGenerationContentV1>>,
    chunks: GenerationChunkManifestV1,
    symbols: GenerationSymbolIndexV1,
    lineage: Vec<SymbolLineageCandidateV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    edges: Vec<CanonicalRelationEdgeV1>,
    /// Canonical unresolved-reference limitations derived while sealing:
    /// `Calls` rows are call sites without an edge, `Implements` rows are Go
    /// interfaces whose implementors the seal could not decide.
    unresolved_calls: Vec<CodeIndexUnresolvedReferenceV1>,
    edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
    statistics: CodeIndexGenerationStatisticsV1,
    clone_payloads_reused: u64,
    clone_payloads_computed: u64,
    clone_stale_invalidations: u64,
    coverage: CoverageSummaryV1,
    capability: CodeIndexCapabilityManifestV1,
    projection: ProjectionPublicationHandoffV1,
    /// Amortized integrity gate. A generation is immutable once constructed, so
    /// the canonical manifest/chunk/graph/capability checks are a pure function
    /// of the fields above and only need to run once per in-memory generation.
    ///
    /// Fail-closed by construction: only a *successful* validation is recorded,
    /// every generation starts unvalidated, and a failing generation re-runs the
    /// full check on every call. Clones inherit the mark because a clone is
    /// deep-equal to an already-verified value.
    validated: OnceLock<()>,
    /// Reclaimable parser-backed exact-admission staging. `admit_all`
    /// re-canonicalizes and re-hashes every chunk, so concurrent consumers
    /// share one build while any consumer still owns it. Once the retained
    /// exact and lexical query owners have consumed the staging corpus, the
    /// weak memo lets its duplicate chunk allocation be reclaimed.
    admitted: OnceLock<Arc<Mutex<Weak<Vec<ExtractionAdmittedCodeSearchChunkV1>>>>>,
    /// Amortized test-attribution join. The traversal and its evidence digest
    /// are a pure function of the immutable generation. Only success is cached.
    attribution: Arc<OnceLock<PublishedGenerationTestAttributionAuthorityV1>>,
    /// Serialize fallible cold construction without blocking cached reads or
    /// resident accounting behind it. Failures leave the memo uninitialized.
    attribution_build: Arc<Mutex<ProviderEvaluationStateV1>>,
    /// Amortized chunk policy-revision census. Owner-compatibility dispatch
    /// needs the one policy revision the chunks were sealed under; scanning
    /// every chunk on each `active_generation` call re-derived a value that is
    /// a pure function of the immutable generation.
    chunk_policy: OnceLock<ChunkPolicyRevisionSummaryV1>,
    /// [`Self::retained_bytes`] of the immutable decode, measured once.
    retained_bytes: OnceLock<u64>,
    /// How far this process's unreclaimable resident bytes rose above their
    /// starting point while this generation was decoded, sampled at every
    /// decode pass boundary. `None` for a generation built in memory, or when
    /// the kernel reports no resident set.
    decode_peak_growth_bytes: Option<u64>,
}

/// The chunk policy-revision census of one immutable generation: no chunks at
/// all, one uniform revision, or disagreeing revisions (which no owner
/// configuration can ever be compatible with).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChunkPolicyRevisionSummaryV1 {
    Empty,
    Uniform(PolicyRevisionId),
    Mixed,
}

impl ChunkPolicyRevisionSummaryV1 {
    /// The census of a generation whose chunks are this census's plus
    /// chunks sealed under `revision`.
    pub(crate) fn with_chunks_under(&self, revision: &PolicyRevisionId) -> Self {
        match self {
            Self::Empty => Self::Uniform(revision.clone()),
            Self::Uniform(current) if current == revision => self.clone(),
            Self::Uniform(_) | Self::Mixed => Self::Mixed,
        }
    }

    /// Insert the incompatibility this census implies for `config`.
    fn observe(
        &self,
        config: &CodeIndexProductionConfigV1,
        compatibility: &mut CodeIndexGenerationCompatibilityV1,
    ) {
        match self {
            Self::Empty => {}
            Self::Uniform(revision) if *revision != config.policy_revision => {
                compatibility
                    .incompatibilities
                    .insert(CodeIndexGenerationIncompatibilityV1::PolicyRevision);
            }
            Self::Mixed => {
                compatibility
                    .incompatibilities
                    .insert(CodeIndexGenerationIncompatibilityV1::MixedPolicyRevisions);
            }
            Self::Uniform(_) => {}
        }
    }
}

impl CodeIndexPublishedGenerationV1 {
    /// Stage a validated decoded or restored generation through the canonical
    /// writer. The caller retains its decoded input; the returned publication
    /// owns only authenticated metadata and its projection proof. This does
    /// not activate the generation or bypass the writer's final source/CAS fence.
    pub fn stage_publication(
        &self,
        preparation: &mut impl CodeIndexPreparationPort,
    ) -> Result<CodeIndexSealedPublicationV1, CodeIndexProductionErrorV1> {
        let manifest = self.encode_partitioned_sealed(|segment| {
            preparation.write_segment(segment).map_err(Into::into)
        })?;
        CodeIndexSealedPublicationV1::from_prepared(
            Arc::from(manifest),
            self.projection.clone(),
            None,
            (0, 0, 0),
        )
    }

    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.manifest
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.snapshot
    }

    /// The exact generation scope this generation was sealed under:
    /// repository, sealed branch label, and worktree.
    ///
    /// This, never a filesystem path and never the generation id, is the
    /// key that partitions active-generation slots and code shards, so a
    /// sealed generation can only ever be dispatched onto the scope whose
    /// snapshot sealed it.
    pub fn sealed_scope(&self) -> CodeIndexGenerationScopeV1 {
        CodeIndexGenerationScopeV1::for_snapshot(&self.snapshot)
    }

    pub fn chunks(&self) -> &GenerationChunkManifestV1 {
        &self.chunks
    }

    pub fn symbols(&self) -> &GenerationSymbolIndexV1 {
        &self.symbols
    }

    pub fn lineage(&self) -> &[SymbolLineageCandidateV1] {
        &self.lineage
    }

    pub fn imports(&self) -> &[CodeIndexImportEvidenceV1] {
        &self.imports
    }

    pub fn schema_evidence(&self) -> impl Iterator<Item = &ExtractedSchemaEvidenceV1> {
        self.files
            .iter()
            .filter_map(|file| file.artifacts.schema_evidence.as_ref())
    }

    pub fn unresolved_references(
        &self,
    ) -> impl Iterator<Item = (&str, &CodeIndexUnresolvedReferenceV1)> {
        self.files.iter().flat_map(|file| {
            file.artifacts
                .unresolved_references
                .iter()
                .map(|reference| (file.authority.logical_path.as_str(), reference))
        })
    }

    /// Call sites whose import binding names project code the seal could not
    /// bind; see [`helpers::unresolved_import_calls`].
    pub fn unresolved_import_calls(
        &self,
    ) -> Result<Vec<CodeIndexUnresolvedReferenceV1>, CodeIndexProductionErrorV1> {
        unresolved_import_calls(
            &self.files,
            &resolution_view::FileSymbolsByNameV1::new(&self.files)?,
            None,
        )
    }

    pub fn analysis_coverage(&self) -> impl Iterator<Item = (&str, &ExtractionBatchV1)> {
        self.files
            .iter()
            .map(|file| (file.authority.logical_path.as_str(), &file.extraction))
    }

    pub fn edges(&self) -> &[CanonicalRelationEdgeV1] {
        &self.edges
    }

    pub fn edge_abstentions(&self) -> &[CodeIndexEdgeAbstentionV1] {
        &self.edge_abstentions
    }

    pub fn coverage(&self) -> &CoverageSummaryV1 {
        &self.coverage
    }

    pub fn capability(&self) -> &CodeIndexCapabilityManifestV1 {
        &self.capability
    }

    pub fn projection(&self) -> &ProjectionPublicationHandoffV1 {
        &self.projection
    }

    /// Whether this in-memory generation has already passed its canonical
    /// integrity validation.
    ///
    /// A generation only reports `true` after a full successful check, so this
    /// distinguishes an amortized O(1) admission from a first verification. It
    /// never short-circuits the gate: an unvalidated generation still refuses
    /// to seal or serve until the complete check passes.
    pub fn is_validated(&self) -> bool {
        self.validated.get().is_some()
    }

    /// Whether this generation's parser-backed exact-admission staging still
    /// has a live consumer.
    ///
    /// [`Self::admitted_chunks`] re-canonicalizes and re-hashes every chunk on
    /// its first call, so concurrent callers share a single build. The memo is
    /// deliberately weak: persistent query owners retain their serving
    /// projections, not this duplicate staging corpus. Like
    /// [`Self::is_validated`] it reports memo state only and never
    /// short-circuits a gate.
    pub fn is_exact_admission_warm(&self) -> bool {
        self.admitted.get().is_some_and(|admitted| {
            let admitted = match admitted.lock() {
                Ok(admitted) => admitted,
                Err(poisoned) => poisoned.into_inner(),
            };
            admitted.strong_count() > 0
        })
    }

    /// The chunk policy-revision census, computed once per in-memory
    /// generation. Chunks are immutable after construction, so the census is
    /// a pure function of the generation and owner-compatibility checks
    /// reduce to one comparison instead of an O(chunks) scan per call.
    fn chunk_policy_summary(&self) -> &ChunkPolicyRevisionSummaryV1 {
        self.chunk_policy.get_or_init(|| {
            let mut chunks = self.chunks.chunks().iter();
            let Some(first) = chunks.next() else {
                return ChunkPolicyRevisionSummaryV1::Empty;
            };
            if chunks
                .any(|chunk| chunk.sensitivity.policy_revision != first.sensitivity.policy_revision)
            {
                ChunkPolicyRevisionSummaryV1::Mixed
            } else {
                ChunkPolicyRevisionSummaryV1::Uniform(first.sensitivity.policy_revision.clone())
            }
        })
    }

    /// Compare every owner-controlled generation input against this immutable
    /// seal and return one canonical compatibility witness.
    #[tracing::instrument(
        name = "code_index.generation.compatibility",
        level = "trace",
        skip_all
    )]
    pub fn compatibility_with(
        &self,
        config: &CodeIndexProductionConfigV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let mut compatibility = CodeIndexGenerationCompatibilityV1::for_metadata(
            &self.manifest,
            &self.snapshot,
            config,
        );
        self.chunk_policy_summary()
            .observe(config, &mut compatibility);
        compatibility
    }

    /// Bytes this decoded generation holds: the decode itself plus the test
    /// attribution once built. The decode is summed from the lengths of its
    /// own allocations (every chunk's text, terms, subtokens and identifiers,
    /// every clone body's token streams, every symbol, edge and per-file
    /// record); containers count their element slots. Allocator headers and
    /// spare capacity are not visible here, so this is a floor of the true
    /// resident cost, never an extrapolation above it.
    #[must_use]
    pub fn retained_bytes(&self) -> u64 {
        let decode = *self
            .retained_bytes
            .get_or_init(|| u64::try_from(self.measure_decode_bytes()).unwrap_or(u64::MAX));
        let attribution = self.attribution.get().map_or(
            0,
            PublishedGenerationTestAttributionAuthorityV1::retained_bytes,
        );
        decode.saturating_add(attribution)
    }

    /// What decoding this generation from its sealed bytes cost at its peak,
    /// as measured when this copy was decoded; `None` for a generation built
    /// in memory. Admission charges this, not [`Self::retained_bytes`], for
    /// the next decode of the same generation.
    #[must_use]
    pub fn decode_peak_growth_bytes(&self) -> Option<u64> {
        self.decode_peak_growth_bytes
    }

    fn measure_decode_bytes(&self) -> usize {
        self.measure_resident_bytes()
    }

    /// The decoded pages this generation shares with every generation that
    /// sealed the same content; `None` for a generation built in process.
    /// Their bytes are part of [`Self::retained_bytes`].
    #[must_use]
    pub fn shared_content(&self) -> Option<&Arc<DecodedGenerationContentV1>> {
        self.content.as_ref()
    }

    /// Read the immutable authority without joining cold construction. The
    /// mounted worker owns preparation; a warming or failed generation never
    /// masquerades as a completed empty join.
    pub fn test_attribution_read(&self) -> Arc<GenerationProviderReadV1<GenerationTestJoinV1>> {
        if let Some(attribution) = self.attribution.get() {
            return Arc::clone(&attribution.read);
        }
        let provider_state = match self.attribution_build.try_lock() {
            Ok(state) => *state,
            Err(std::sync::TryLockError::WouldBlock) => ProviderEvaluationStateV1::Indexing,
            Err(std::sync::TryLockError::Poisoned(_)) => ProviderEvaluationStateV1::Failed,
        };
        Arc::new(GenerationProviderReadV1 {
            provider_state,
            coverage: GenerationProviderCoverageV1::Unavailable,
            evidence: None,
        })
    }

    /// Prepare once under the generation owner, retaining failures as typed
    /// read states. An explicit subsequent preparation may retry a failure.
    pub fn prepare_test_attribution(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<PublishedGenerationTestAttributionAuthorityV1, CodeIndexProductionErrorV1> {
        if let Some(attribution) = self.attribution.get() {
            return Ok(attribution.clone());
        }
        let mut state = self.attribution_build.lock().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "test attribution construction lock is poisoned: {error}"
            ))
        })?;
        if let Some(attribution) = self.attribution.get() {
            return Ok(attribution.clone());
        }
        *state = ProviderEvaluationStateV1::Indexing;
        match self.build_test_attribution_authority(control) {
            Ok(authority) => {
                let _ = self.attribution.set(authority.clone());
                Ok(authority)
            }
            Err(error) => {
                *state = match &error {
                    CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled) => {
                        ProviderEvaluationStateV1::Cancelled
                    }
                    CodeIndexProductionErrorV1::Interrupted(
                        CodeIndexInterruptionV1::DeadlineExceeded,
                    ) => ProviderEvaluationStateV1::TimedOut,
                    _ => ProviderEvaluationStateV1::Failed,
                };
                Err(error)
            }
        }
    }

    #[tracing::instrument(name = "code_index.test_attribution.build", level = "trace", skip_all)]
    fn build_test_attribution_authority(
        &self,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<PublishedGenerationTestAttributionAuthorityV1, CodeIndexProductionErrorV1> {
        lexical_page_source::checkpoint(control)?;
        let indexing = tracing::trace_span!("code_index.test_attribution.index").entered();
        let mut file_by_occurrence = HashMap::new();
        for file in &self.snapshot.files {
            file_by_occurrence.insert(
                file.file_occurrence_id.clone(),
                (file.logical_path.as_str(), file.content_digest.clone()),
            );
        }

        let mut occurrence_files: HashMap<
            SymbolOccurrenceId,
            (FileOccurrenceId, tracedecay_domain::ContentDigest),
        > = HashMap::new();
        for chunk in self.chunks.chunks() {
            lexical_page_source::checkpoint(control)?;
            let Some(occurrence) = &chunk.anchor.symbol_occurrence_id else {
                continue;
            };
            let Some((_, content_digest)) =
                file_by_occurrence.get(&chunk.anchor.file_occurrence_id)
            else {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "test attribution chunk refers to a missing snapshot file".to_owned(),
                ));
            };
            match occurrence_files.entry(occurrence.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((
                        chunk.anchor.file_occurrence_id.clone(),
                        content_digest.clone(),
                    ));
                }
                std::collections::hash_map::Entry::Occupied(entry)
                    if entry.get().0 != chunk.anchor.file_occurrence_id =>
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "test attribution occurrence crosses snapshot files".to_owned(),
                    ));
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }

        let mut occurrence_files = occurrence_files.into_iter().collect::<Vec<_>>();
        occurrence_files.sort_unstable_by(|left, right| left.0.cmp(&right.0));

        let callable_occurrences = self
            .symbols
            .symbols
            .iter()
            .filter(|symbol| {
                matches!(
                    symbol.kind.as_str(),
                    "function"
                        | "method"
                        | "struct_method"
                        | "abstract_method"
                        | "constructor"
                        | "arrow_function"
                        | "procedure"
                )
            })
            .map(|symbol| &symbol.occurrence)
            .collect::<HashSet<_>>();
        let test_markers = self
            .symbols
            .symbols
            .iter()
            .filter(|symbol| crate::is_test_marker(symbol))
            .map(|symbol| &symbol.occurrence)
            .collect::<HashSet<_>>();
        let annotated_test_occurrences = self
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == RelationEdgeKindV1::Annotates
                    && test_markers.contains(&edge.from_occurrence)
            })
            .map(|edge| &edge.to_occurrence)
            .collect::<HashSet<_>>();
        let test_occurrences = occurrence_files
            .iter()
            .enumerate()
            .filter_map(|(index, (occurrence, (file, _)))| {
                callable_occurrences.contains(occurrence).then_some(())?;
                (annotated_test_occurrences.contains(occurrence)
                    || file_by_occurrence
                        .get(file)
                        .is_some_and(|(path, _)| crate::is_test_file(path)))
                .then(|| (occurrence.clone(), index))
            })
            .collect::<Vec<_>>();
        // Canonical indices preserve occurrence ordering while keeping string
        // cloning and tree lookups out of each test's transitive traversal.
        let occurrences_by_index = occurrence_files
            .iter()
            .map(|(occurrence, _)| occurrence)
            .collect::<Vec<_>>();
        let occurrence_indices = occurrences_by_index
            .iter()
            .enumerate()
            .map(|(index, occurrence)| (*occurrence, index))
            .collect::<HashMap<_, _>>();
        let mut outgoing = vec![Vec::new(); occurrences_by_index.len()];
        for edge in &self.edges {
            lexical_page_source::checkpoint(control)?;
            if let (Some(&from), Some(&to)) = (
                occurrence_indices.get(&edge.from_occurrence),
                occurrence_indices.get(&edge.to_occurrence),
            ) {
                outgoing[from].push(to);
            }
        }
        for destinations in &mut outgoing {
            destinations.sort_unstable();
            destinations.dedup();
        }

        let attribution_revision =
            ComponentVersion::new("code-index.test-attribution.conservative.v1")
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        drop(indexing);
        let traversal = tracing::trace_span!("code_index.test_attribution.traverse").entered();
        let mut attributions = Vec::with_capacity(test_occurrences.len());
        let mut visited = vec![usize::MAX; occurrences_by_index.len()];
        let mut covered = Vec::new();
        for (visit, (test_occurrence, test_index)) in test_occurrences.into_iter().enumerate() {
            lexical_page_source::checkpoint(control)?;
            covered.clear();
            covered.push(test_index);
            visited[test_index] = visit;
            let mut cursor = 0;
            while cursor < covered.len() {
                lexical_page_source::checkpoint(control)?;
                for &destination in &outgoing[covered[cursor]] {
                    if visited[destination] != visit {
                        visited[destination] = visit;
                        covered.push(destination);
                    }
                }
                cursor += 1;
            }
            covered.sort_unstable();
            attributions.push(GenerationTestAttributionV1 {
                generation_id: self.manifest.generation_id.clone(),
                source_revision: self.snapshot.source_revision.clone(),
                test_occurrence,
                covered_occurrences: covered
                    .iter()
                    .map(|&index| occurrences_by_index[index].clone())
                    .collect(),
                evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
                attribution_revision: attribution_revision.clone(),
            });
        }

        drop(traversal);
        let occurrences = occurrence_files
            .into_iter()
            .map(|(occurrence_id, (file_occurrence_id, content_digest))| {
                TestAttributionOccurrenceV1 {
                    occurrence_id,
                    file_occurrence_id,
                    content_digest,
                }
            })
            .collect::<Vec<_>>();
        let unknown = self.edge_abstentions.len() as u64
            + self.coverage.files_partial
            + self.coverage.files_unsupported
            + self.coverage.ranges_unsupported;
        let input_coverage = if unknown == 0 {
            TestAttributionJoinInputCoverageV1::Complete
        } else {
            TestAttributionJoinInputCoverageV1::Partial {
                reason: "canonical graph or source coverage is incomplete".to_owned(),
            }
        };
        let eligible = attributions.len() as u64;
        lexical_page_source::checkpoint(control)?;
        let joining = tracing::trace_span!("code_index.test_attribution.join").entered();
        let snapshot = ValidatedCodeSnapshotV1 {
            snapshot: self.snapshot.clone(),
            intake_digest: self.manifest.snapshot_digest.clone(),
            validated_at: self.manifest.seal.sealed_at,
        };
        let join = GenerationTestJoinV1::produce(
            &self.manifest,
            &snapshot,
            attributions,
            &occurrences,
            attribution_revision,
            input_coverage,
            &|| control.is_cancelled() || control.is_deadline_exceeded(),
        )
        .map_err(|error| match error {
            GenerationTestJoinErrorV1::Interrupted => interruption_error(control),
            other => CodeIndexProductionErrorV1::Contract(other.to_string()),
        })?;
        drop(joining);
        lexical_page_source::checkpoint(control)?;
        let (provider_state, coverage) = if unknown == 0 {
            (
                ProviderEvaluationStateV1::SupportedCompletedComplete,
                GenerationProviderCoverageV1::Complete {
                    examined: eligible,
                    eligible,
                    excluded: 0,
                },
            )
        } else {
            (
                ProviderEvaluationStateV1::Partial,
                GenerationProviderCoverageV1::Partial {
                    examined: eligible.saturating_add(unknown),
                    eligible,
                    excluded: 0,
                    unknown,
                    capped: false,
                },
            )
        };
        let read = GenerationProviderReadV1::new(provider_state, coverage, Some(join))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let measuring =
            tracing::trace_span!("code_index.test_attribution.retained_bytes").entered();
        let retained_bytes = read
            .evidence
            .as_ref()
            .map_or(0, GenerationTestJoinV1::retained_bytes);
        drop(measuring);
        lexical_page_source::checkpoint(control)?;
        Ok(PublishedGenerationTestAttributionAuthorityV1 {
            generation_id: self.manifest.generation_id.clone(),
            read: Arc::new(read),
            retained_bytes,
        })
    }

    /// Return chunks re-admitted through their parser-backed exact authority.
    /// Downstream exact/phrase/BM25 projections must consume this value rather
    /// than raw chunks, preserving the non-demotable exact tier.
    ///
    /// The admitted sweep is shared while a consumer owns it, then reclaimed
    /// after the persistent query owners have built their serving projections.
    /// Returning an owned `Vec` here deep-copied ~150K chunks (content included)
    /// on every memo hit, which put an O(store) memcpy on every search's request
    /// path. Holding the memo lock through construction preserves single-flight
    /// admission for concurrent cold consumers.
    pub fn admitted_chunks(
        &self,
    ) -> Result<Arc<Vec<ExtractionAdmittedCodeSearchChunkV1>>, ChunkingFailureV1> {
        let admitted = self
            .admitted
            .get_or_init(|| Arc::new(Mutex::new(Weak::new())));
        let mut memo = match admitted.lock() {
            Ok(memo) => memo,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(admitted) = memo.upgrade() {
            return Ok(admitted);
        }
        let mut chunks = Vec::new();
        for file in &self.files {
            chunks.extend(
                file.exact_authority
                    .admit_all(file.artifacts.chunks.chunks.clone())?,
            );
        }
        chunks.sort_by(|left, right| left.chunk().id.cmp(&right.chunk().id));
        let chunks = Arc::new(chunks);
        *memo = Arc::downgrade(&chunks);
        Ok(chunks)
    }

    /// Amortized integrity gate for an already-constructed generation.
    ///
    /// The first call runs every canonical check; later calls are O(1). This is
    /// sound because a published generation is immutable: no field can change
    /// after construction, so re-validating identical bytes cannot change the
    /// answer. It is fail-closed because only success is memoized, a
    /// generation that has never validated still runs the full check, and a
    /// generation that fails keeps failing on every subsequent call.
    pub(crate) fn validate(&self) -> Result<(), CodeIndexProductionErrorV1> {
        if self.validated.get().is_some() {
            return Ok(());
        }
        self.validate_fresh()
    }

    /// Run every canonical check against the current in-memory state, ignoring
    /// any memoized verdict, then record success.
    ///
    /// Use this wherever bytes were genuinely re-read (sealed-generation
    /// restore) so the memoized fast path can never mask a real re-read.
    pub(crate) fn validate_fresh(&self) -> Result<(), CodeIndexProductionErrorV1> {
        self.validate_uncached()?;
        let _ = self.validated.set(());
        Ok(())
    }

    fn validate_uncached(&self) -> Result<(), CodeIndexProductionErrorV1> {
        self.manifest
            .validate()
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let commitments = self
            .manifest
            .source_commitments
            .as_ref()
            .ok_or_else(missing_source_commitments)?;
        let expected_seal = expected_seal_digest(&self.manifest)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if expected_seal != self.manifest.seal.expected_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "code generation manifest seal does not authenticate its source commitments"
                    .to_owned(),
            ));
        }
        if commitments.incremental_manifest_digest
            != self.projection.request().changes.manifest_digest
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "code generation incremental source commitment does not match its projection"
                    .to_owned(),
            ));
        }
        let full_source = self
            .chunks
            .chunks()
            .iter()
            .map(|chunk| (chunk.id.clone(), chunk.content_digest.clone()))
            .collect::<Vec<_>>();
        // A restored successor authenticates its reused complement against
        // its parent's persisted full-replay commitment; a generation built
        // whole lists every chunk.
        self.projection
            .request()
            .changes
            .validate_reused_complement_for_restore(
                commitments.parent_full_replay_digest.as_ref(),
                &full_source,
            )
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        {
            commitments
                .validate_for_changes(&self.projection.request().changes)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        }
        self.ignored_source_roster
            .validate(&self.snapshot, &self.repository_parse_identity)?;
        if self.chunks.generation_id() != &self.manifest.generation_id
            || self.symbols.generation_id != self.manifest.generation_id
            || self.capability.generation_id != self.manifest.generation_id
            || self.projection.source_generation() != &self.manifest.generation_id
            || self.capability.source_coverage != self.coverage
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "published generation mixes immutable generation evidence".to_owned(),
            ));
        }
        self.capability
            .validate()
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;

        let mut files = self.files.clone();
        files.sort_by(|left, right| {
            left.artifacts
                .chunks
                .document
                .file_occurrence_id
                .cmp(&right.artifacts.chunks.document.file_occurrence_id)
        });
        if files.windows(2).any(|pair| {
            pair[0].artifacts.chunks.document.file_occurrence_id
                == pair[1].artifacts.chunks.document.file_occurrence_id
        }) {
            return Err(CodeIndexProductionErrorV1::Contract(
                "published generation repeats a file occurrence".to_owned(),
            ));
        }
        let occurrences_by_id = self
            .snapshot
            .files
            .iter()
            .map(|candidate| (&candidate.file_occurrence_id, candidate))
            .collect::<HashMap<_, _>>();
        {
            let _span = tracing::trace_span!("code_index.collect.validate_files").entered();
            collect_bounded_ordered(&files, |file| {
                {
                    file.artifacts
                        .validate()
                        .map_err(CodeIndexProductionErrorV1::Chunk)?;
                }
                let occurrence = occurrences_by_id
                    .get(&file.artifacts.chunks.document.file_occurrence_id)
                    .copied();
                if file.authority.project_id != self.manifest.project_id {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "published file authority project does not match the generation manifest"
                            .to_owned(),
                    ));
                }
                if file.authority.repository_id != self.snapshot.repository
                    || occurrence.is_none_or(|occurrence| {
                        occurrence.logical_path != file.authority.logical_path
                            || occurrence.content_digest != file.authority.content_digest
                    })
                    || file
                        .artifacts
                        .schema_evidence
                        .as_ref()
                        .is_some_and(|evidence| {
                            evidence.logical_path != file.authority.logical_path
                        })
                    || file.extraction.content_digest != file.authority.content_digest
                    || file.extraction.file_occurrence_id
                        != file.artifacts.chunks.document.file_occurrence_id
                    || file.artifacts.chunks.document.generation_id != file.extraction.generation_id
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                    "extraction authority does not match its published project, repository, scope, path, or content"
                        .to_owned(),
                ));
                }
                file.exact_authority
                    .validate_all(&file.artifacts.chunks.chunks)
                    .map_err(CodeIndexProductionErrorV1::Chunk)?;
                Ok(())
            })
        }?;
        {
            let _span = tracing::trace_span!("code_index.collect.validate_aggregates").entered();
            {
                let file_refs = files.iter().map(Arc::as_ref).collect::<Vec<_>>();
                validate_import_evidence(&file_refs, &self.imports)?;
                let mut file_chunk_count = 0usize;
                let mut file_symbol_count = 0usize;
                for file in &files {
                    file_chunk_count += file.artifacts.chunks.chunks.len();
                    file_symbol_count += file.artifacts.symbols.len();
                }
                if file_chunk_count != self.chunks.chunks().len()
                    || file_symbol_count != self.symbols.symbols.len()
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "published generation does not match file artifacts".to_owned(),
                    ));
                }
                let chunk_ptrs = self
                    .chunks
                    .chunks()
                    .iter()
                    .map(Arc::as_ptr)
                    .collect::<HashSet<_>>();
                for file in &files {
                    for chunk in &file.artifacts.chunks.chunks {
                        if !chunk_ptrs.contains(&Arc::as_ptr(chunk)) {
                            return Err(CodeIndexProductionErrorV1::Contract(
                                "published generation does not match file artifacts".to_owned(),
                            ));
                        }
                    }
                }
                let symbol_ptrs = self
                    .symbols
                    .symbols
                    .iter()
                    .map(Arc::as_ptr)
                    .collect::<HashSet<_>>();
                for file in &files {
                    for symbol in &file.artifacts.symbols {
                        if !symbol_ptrs.contains(&Arc::as_ptr(symbol)) {
                            return Err(CodeIndexProductionErrorV1::Contract(
                                "published generation does not match file artifacts".to_owned(),
                            ));
                        }
                    }
                }
                // Edges are never persisted: every generation, restored or freshly
                // built, owns the vector canonical resolution derived from
                // these same immutable files. Re-deriving it here would compare a
                // deterministic function against itself at the price of a second
                // 1.7M-reference resolution, so the persisted per-file abstentions
                // are what this check can actually falsify.
                let mut edge_abstentions = files
                    .iter()
                    .flat_map(|file| file.artifacts.edge_abstentions.iter())
                    .collect::<Vec<_>>();
                edge_abstentions.sort();
                let abstentions_match = edge_abstentions.len() == self.edge_abstentions.len()
                    && edge_abstentions
                        .iter()
                        .zip(&self.edge_abstentions)
                        .all(|(left, right)| *left == right);
                if !abstentions_match {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "published graph evidence does not match file artifacts".to_owned(),
                    ));
                }
                Ok::<_, CodeIndexProductionErrorV1>(())
            }
        }
    }
}

/// Construction failure for a production owner.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodeIndexProductionOpenErrorV1 {
    #[error("code-index production configuration is invalid")]
    InvalidConfiguration,
    #[error("maximum snapshot age cannot be negative")]
    InvalidSnapshotAge,
}

/// Input evidence that cannot be associated with exactly one sanitized,
/// present snapshot file.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodeIndexInputErrorV1 {
    #[error("the snapshot has no present source files with a supported language")]
    NoExtractableFiles,
    #[error("captured source repeats a file occurrence")]
    DuplicateCapturedFile,
    #[error("a present snapshot file has no captured sanitized bytes")]
    MissingCapturedFile,
    #[error("captured source is absent from the present snapshot files")]
    UnexpectedCapturedFile,
    #[error("captured source does not match its declared content digest")]
    ContentDigestMismatch,
}

/// A typed failure that leaves the previously active generation untouched.
#[derive(Debug, Error)]
pub enum CodeIndexProductionErrorV1 {
    #[error("code indexing was interrupted: {0:?}")]
    Interrupted(CodeIndexInterruptionV1),
    #[error("captured source input is invalid: {0}")]
    Input(#[from] CodeIndexInputErrorV1),
    #[error("sanitized intake rejected the snapshot: {0:?}")]
    Intake(IntakeRejectionV1),
    #[error("generation planning failed: {0}")]
    Generation(GenerationPlanningErrorV1),
    #[error("language extraction failed: {0:?}")]
    Extraction(ExtractionFailureV1),
    #[error("retained Tree-sitter parsing failed: {0}")]
    RetainedParse(#[from] ParseError),
    #[error("chunking failed: {0}")]
    Chunk(ChunkingFailureV1),
    #[error("incremental materialization failed: {0}")]
    Increment(ChunkIncrementErrorV1),
    #[error("lineage construction failed: {0}")]
    Lineage(LineageResolutionErrorV1),
    #[error("capability emission failed: {0}")]
    Capability(CapabilityEmissionErrorV1),
    #[error("projection receipt verification failed: {0}")]
    Projection(ProjectionPublicationErrorV1),
    #[error(transparent)]
    Publication(#[from] CodeIndexPublicationStoreErrorV1),
    /// A sealed generation whose envelope revision this build no longer reads.
    ///
    /// A generation is a pure function of its source tree, so a superseded
    /// envelope is refused rather than migrated: the daemon abstains from the
    /// stale artifact and rebuilds it.
    #[error(
        "sealed generation format revision {0} predates this build; the generation will be rebuilt from source"
    )]
    SupersededSealedGenerationRevision(u32),
    /// A digest-verified sealed row no longer satisfies this build's row
    /// contract (an older writer shape). The bytes are authentic, so this is
    /// a superseded revision to rebuild from source, not corruption.
    #[error("sealed file segment revision {revision} rows are refused by this build: {message}")]
    SealedRowContractRefused { revision: u32, message: String },
    #[error("code-index contract failed: {0}")]
    Contract(String),
    #[error("code-index parallel worker runtime failed: {0}")]
    Parallelism(#[from] crate::parallelism::CodeIndexParallelismErrorV1),
}

/// Every sealed manifest carries the source commitments its seal
/// authenticates; only the planner holds a manifest without them.
pub(crate) fn missing_source_commitments() -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(
        "sealed code generation manifest carries no source commitments".to_owned(),
    )
}

/// Slot lookup for one publish attempt.
///
/// `reusable` is the generation increment planning may adopt. `cas_incumbent`
/// is the compare-and-swap expected token: the same id when reuse is legal,
/// or the prior-label incumbent after a same-checkout label move that must
/// rebuild while still replacing the worktree slot atomically.
struct ActiveGenerationLookupV1 {
    reusable: Option<SealedParentGenerationV1>,
    cas_incumbent: Option<CodeGenerationId>,
}

/// Production owner for one repository and one atomic publication authority.
pub struct CodeIndexProductionOwnerV1<P: CodeIndexAtomicPublicationPort> {
    config: CodeIndexProductionConfigV1,
    publication: P,
    unpublished: Option<(P::Preparation, CodeIndexSealedPublicationV1)>,
    reconstruction_requirement: Option<ResidentMemoryReconstructionDemandV1>,
    physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    retained_parses: SharedRetainedParsePool,
}

impl<P> CodeIndexProductionOwnerV1<P>
where
    P: CodeIndexAtomicPublicationPort,
{
    pub fn new(
        config: CodeIndexProductionConfigV1,
        publication: P,
    ) -> Result<Self, CodeIndexProductionOpenErrorV1> {
        config.validate()?;
        Ok(Self {
            config,
            publication,
            unpublished: None,
            reconstruction_requirement: None,
            physical_artifacts: SharedPhysicalCodeArtifactPoolV1::default(),
            retained_parses: SharedRetainedParsePool::default(),
        })
    }

    pub fn with_physical_artifact_pool(
        mut self,
        physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    ) -> Self {
        self.physical_artifacts = physical_artifacts;
        self
    }

    pub fn with_retained_parse_pool(mut self, retained_parses: SharedRetainedParsePool) -> Self {
        self.retained_parses = retained_parses;
        self
    }

    /// Rebind attempt-scoped publication fences on the existing physical
    /// store handle while retaining this owner's parser and artifact pools.
    pub fn publication_mut(&mut self) -> &mut P {
        &mut self.publication
    }

    pub fn retained_parse_stats(&self) -> RetainedParsePoolStats {
        self.retained_parses.stats()
    }

    /// The documents increments retain, as a handle the resident-memory
    /// inventory samples and releases.
    pub fn retained_parse_pool(&self) -> SharedRetainedParsePool {
        self.retained_parses.clone()
    }

    pub fn physical_artifact_pool_stats(&self) -> PhysicalCodeArtifactPoolStatsV1 {
        self.physical_artifacts.stats()
    }

    /// Load the currently reusable immutable generation. A restart therefore
    /// resumes from the publication authority rather than mutable worker state.
    ///
    /// Reuse is full-scope exact: the loaded generation must have been sealed
    /// under the requested repository, reference, and worktree. A same-checkout
    /// reference label move is a rebuild (`Ok(None)`), not reuse, the
    /// worktree-scoped slot still holds the prior label's incumbent, which
    /// [`Self::build_and_publish`] keeps as the compare-and-swap expected
    /// token. A publication authority that answers a scope with a generation
    /// sealed for a *foreign checkout* has broken its slot partition, or the
    /// caller's checkout identity resolution regressed, e.g. a repository
    /// misclassified as not-a-git-path. That is the terminal
    /// [`CodeIndexPublicationStoreErrorV1::CorruptionResetRequired`] state:
    /// the foreign generation is never adopted, never config-checked, and the
    /// refusal is a reset journey, not a transient error to retry on a
    /// cadence.
    pub fn active_generation(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexProductionErrorV1> {
        Ok(self
            .lookup_active_generation(scope)?
            .reusable
            .map(|parent| parent.sealed().clone()))
    }

    #[tracing::instrument(name = "code_index.build.active_generation", level = "trace", skip_all)]
    fn lookup_active_generation(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<ActiveGenerationLookupV1, CodeIndexProductionErrorV1> {
        scope.validate()?;
        if scope.repository != self.config.repository {
            return Err(CodeIndexProductionErrorV1::Contract(
                "generation scope is foreign to the production owner's repository store".to_owned(),
            ));
        }
        let Some(active) = self.publication.load_active(scope)? else {
            return Ok(ActiveGenerationLookupV1 {
                reusable: None,
                cas_incumbent: None,
            });
        };
        let active = SealedParentGenerationV1::open(active)?;
        let sealed_scope = CodeIndexGenerationScopeV1::for_snapshot(active.snapshot());
        let incumbent = active.manifest().generation_id.clone();
        if sealed_scope != *scope {
            // A reference label moving under the same checkout is a
            // rebuild, not corruption: the worktree-scoped active slot
            // legitimately still points at the generation sealed for the
            // previous label. Publish must still CAS against that
            // incumbent. Only a foreign checkout in the slot is a
            // reset-worthy identity violation.
            if sealed_scope.identifies_same_checkout(scope) {
                return Ok(ActiveGenerationLookupV1 {
                    reusable: None,
                    cas_incumbent: Some(incumbent),
                });
            }
            return Err(CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(format!(
                    "the active-generation slot for {} returned a generation sealed for {}",
                    describe_scope(scope),
                    describe_scope(&sealed_scope),
                )),
            ));
        }
        // Extractor revisions may change persisted row identity. Reject stale
        // artifacts before any of the parent is read.
        let mut compatibility = CodeIndexGenerationCompatibilityV1::for_metadata(
            active.manifest(),
            active.snapshot(),
            &self.config,
        );
        active
            .chunk_policy()
            .observe(&self.config, &mut compatibility);
        Ok(ActiveGenerationLookupV1 {
            reusable: compatibility.is_reusable().then_some(active),
            cas_incumbent: Some(incumbent),
        })
    }

    /// Build one generation and atomically publish it only after intake,
    /// parser evidence, lineage, exact admission, projection receipt, and
    /// capability validation have all succeeded.
    ///
    /// A successor of a compatible sealed parent is built over that parent
    /// from the files the edit changed and never decodes it; a generation
    /// without one, or an edit that needs every file's resolution, is built
    /// whole.
    #[tracing::instrument(name = "code_index.build.and_publish", level = "trace", skip_all)]
    pub fn build_and_publish(
        &mut self,
        request: CodeIndexBuildRequestV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexPublishedBuildV1, CodeIndexProductionErrorV1> {
        // Starting construction supersedes any unclaimed commit retry. A
        // caller retaining that work takes the owned pair instead.
        drop(self.unpublished.take());
        let operation = request.resident_memory.begin_operation();
        let ceiling = request.resident_memory.limit_bytes();
        self.reconstruction_requirement = None;
        let result = self.build_in_operation(request, control, &operation);
        if result.is_err() {
            self.reconstruction_requirement = operation
                .witnessed_reconstruction_requirement(ceiling)
                .map_err(|failure| CodeIndexProductionErrorV1::Contract(failure.to_string()))?;
        }
        result
    }

    /// The construction requirement witnessed before its regions unwind.
    /// Callers apply it only when discarding construction; retaining the
    /// preparation retries commit without reacquiring these bytes.
    pub fn take_reconstruction_requirement(
        &mut self,
    ) -> Option<ResidentMemoryReconstructionDemandV1> {
        self.reconstruction_requirement.take()
    }

    fn build_in_operation(
        &mut self,
        request: CodeIndexBuildRequestV1,
        control: &dyn CodeIndexExecutionControlV1,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<CodeIndexPublishedBuildV1, CodeIndexProductionErrorV1> {
        crate::observe::record_generation_state("building");
        crate::observe::record_rebuild_state("unknown");
        lexical_page_source::checkpoint(control)?;
        let ignored_source_roster = IgnoredSourceRosterV1::admit(
            &request.snapshot,
            &request.repository_parse_identity,
            &request.ignored_source_admissions,
        )?;
        let scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
        let lookup = self.lookup_active_generation(&scope)?;
        let parent = lookup.reusable;
        lexical_page_source::checkpoint(control)?;

        let intake = self.intake_at(request.sealed_at, registry_for_snapshot(&request.snapshot)?);
        let capability = intake
            .admit(request.snapshot.clone())
            .map_err(CodeIndexProductionErrorV1::Intake)?;
        let validated = capability.snapshot().clone();
        let captured_files = captured_files(&validated.snapshot, request.captured_files)?;

        lexical_page_source::checkpoint(control)?;

        let planner = GenerationPlanner::new(
            self.config.project_id.clone(),
            self.config.repository.clone(),
            registry_for_snapshot(&validated.snapshot)?,
            self.config.chunker_revision.clone(),
            self.config.privacy_domain.clone(),
            self.config.privacy_key_epoch,
        );
        let (manifest, increment) = match parent.as_ref() {
            Some(parent) => {
                let plan = planner
                    .plan_increment_with_invalidation(
                        parent.manifest(),
                        parent.snapshot(),
                        &validated,
                        &request.changed_files,
                        &request.invalidations,
                    )
                    .map_err(CodeIndexProductionErrorV1::Generation)?;
                let triggers = plan
                    .rebuild_triggers
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>();
                let manifest = planner
                    .plan_generation_with_invalidation(
                        &validated,
                        Some(parent.manifest()),
                        &triggers,
                        request.sealed_at,
                    )
                    .map_err(CodeIndexProductionErrorV1::Generation)?;
                if manifest.invalidation_digest != plan.invalidation_digest {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "increment plan and generation seal disagree".to_owned(),
                    ));
                }
                (manifest, Some(plan))
            }
            None => (
                planner
                    .plan_generation_with_invalidation(
                        &validated,
                        None,
                        &request.invalidations,
                        request.sealed_at,
                    )
                    .map_err(CodeIndexProductionErrorV1::Generation)?,
                None,
            ),
        };
        lexical_page_source::checkpoint(control)?;

        let extractor = TreeSitterExtractor::new();
        let chunker = DeterministicCodeChunker::new(
            manifest.generation_id.clone(),
            self.config.repository.clone(),
            self.config.sanitizer_revision.clone(),
            self.config.policy_revision.clone(),
            self.config.chunker_revision.clone(),
        );
        crate::observe::record_generation_state(if parent.is_some() {
            "resume"
        } else {
            "initial"
        });
        let mut preparation = self
            .publication
            .prepare(&scope, lookup.cas_incumbent.as_ref())?;
        let cold_reason = match (parent.as_ref(), increment.as_ref()) {
            (Some(parent), Some(plan)) => {
                let sparse = SparseBuildV1 {
                    config: &self.config,
                    physical_artifacts: &self.physical_artifacts,
                    retained_parses: &self.retained_parses,
                    intake: &intake,
                    capability: &capability,
                    extractor: &extractor,
                    chunker: &chunker,
                    repository_parse_identity: &request.repository_parse_identity,
                    ignored_source_roster: &ignored_source_roster,
                    captured_files: &captured_files,
                    target_projection_key: &request.target_projection_key,
                    construction_operation: operation,
                    control,
                }
                .build(
                    parent,
                    plan,
                    manifest.clone(),
                    &validated.snapshot,
                    &mut preparation,
                )?;
                match sparse {
                    Ok(sparse) => {
                        crate::observe::record_rebuild_state("increment");
                        lexical_page_source::checkpoint(control)?;
                        return self.publish(preparation, sparse);
                    }
                    Err(reason) => reason,
                }
            }
            _ => CodeIndexColdBuildReasonV1::NoParent,
        };
        crate::observe::record_rebuild_state("full");
        lexical_page_source::checkpoint(control)?;
        let publication = self.materialize_cold(
            &intake,
            &capability,
            manifest,
            &extractor,
            &chunker,
            request.repository_parse_identity,
            ignored_source_roster,
            &validated.snapshot,
            parent.as_ref(),
            captured_files,
            request.target_projection_key,
            operation.clone(),
            cold_reason,
            &mut preparation,
            control,
        )?;
        lexical_page_source::checkpoint(control)?;
        self.publish(preparation, publication)
    }

    /// A completed construction whose final durable commit can be retried.
    /// The preparation and its authenticated publication always move together.
    pub fn take_unpublished(&mut self) -> Option<(P::Preparation, CodeIndexSealedPublicationV1)> {
        self.unpublished.take()
    }

    fn publish(
        &mut self,
        mut preparation: P::Preparation,
        publication: CodeIndexSealedPublicationV1,
    ) -> Result<CodeIndexPublishedBuildV1, CodeIndexProductionErrorV1> {
        let manifest_bytes = {
            let _span = tracing::trace_span!("code_index.build.publish").entered();
            self.publication.commit(&mut preparation, &publication)
        };
        match manifest_bytes {
            Ok(bytes) => {
                let sealed = CodeIndexSealedGenerationV1::new(bytes, preparation.segment_reader());
                let published = CodeIndexPublishedBuildV1::new(publication, sealed)?;
                crate::observe::record_generation_state("queryable");
                Ok(published)
            }
            Err(error) => {
                if matches!(
                    error,
                    CodeIndexPublicationStoreErrorV1::Unavailable(_)
                        | CodeIndexPublicationStoreErrorV1::StoreLockContended
                        | CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
                ) {
                    self.unpublished = Some((preparation, publication));
                }
                Err(error.into())
            }
        }
    }

    fn intake_at(
        &self,
        reference_time: UtcMicros,
        registry: StaticLanguageRegistry,
    ) -> SanitizedCodeIntake<StaticLanguageRegistry> {
        let intake = SanitizedCodeIntake::new(
            registry,
            self.config.sanitizer_revision.clone(),
            reference_time,
        );
        match self.config.max_snapshot_age_micros {
            Some(max_age) => intake.with_max_snapshot_age_micros(max_age),
            None => intake,
        }
    }

    /// Stage each bounded extraction window before advancing. Full artifacts
    /// never span windows; only authenticated chunk rows and binding facts do.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(name = "code_index.build.materialize_full", level = "trace", skip_all)]
    fn materialize_cold(
        &self,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        mut manifest: CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
        ignored_source_roster: IgnoredSourceRosterV1,
        snapshot: &SanitizedCodeSnapshotV1,
        parent: Option<&SealedParentGenerationV1>,
        mut captured_files: BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        target_projection_key: ProjectionKeyV1,
        operation: ResidentMemoryOperationV1,
        cold_reason: CodeIndexColdBuildReasonV1,
        preparation: &mut P::Preparation,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexSealedPublicationV1, CodeIndexProductionErrorV1> {
        let mut memory = ConstructionMemoryV1::new(operation)?;
        let present_count = snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .count();
        let extraction_span =
            tracing::trace_span!("code_index.cold.extraction", files = present_count,).entered();
        tracing::trace!(
            phase = "extraction",
            files = present_count,
            "cold build phase started"
        );
        let descriptor_bound = present_count
            .saturating_mul(
                std::mem::size_of::<partitioned_codec::PartitionedFileSegmentDescriptorV1>()
                    + 2 * 71,
            )
            .saturating_add(
                snapshot
                    .files
                    .iter()
                    .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
                    .map(|file| file.file_occurrence_id.as_str().len())
                    .sum::<usize>(),
            );
        let extraction_base = descriptor_bound
            .saturating_add(
                present_count.saturating_mul(std::mem::size_of::<(usize, &SanitizedCodeFileV1)>()),
            )
            .saturating_add(manifest.project_id.as_str().len())
            .saturating_add(snapshot.repository.as_str().len())
            // Advancing the uniform policy census clones its one identity
            // before the previous census value is dropped.
            .saturating_add(self.config.policy_revision.as_str().len().saturating_mul(2));
        memory.resize(extraction_base as u64)?;
        let mut present = Vec::with_capacity(present_count);
        present.extend(
            snapshot
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| file.disposition == SnapshotFileDispositionV1::Present),
        );
        let mut file_segments = Vec::with_capacity(present_count);
        let mut chunk_count = 0_usize;
        let scope = sealed_codec::FileScopeIdentityV1::of(&manifest, snapshot);
        let width = crate::parallelism::indexing_workers().max(1);
        let mut chunk_policy = ChunkPolicyRevisionSummaryV1::Empty;
        let mut clones_reused = 0_u64;
        let mut clones_computed = 0_u64;
        let worker_operation = memory.operation.clone();
        let config = &self.config;
        let physical_artifacts = &self.physical_artifacts;
        let retained_parses = &self.retained_parses;
        for window in present.chunks(width) {
            lexical_page_source::checkpoint(control)?;
            let artifacts = collect_bounded_ordered(window, |(key, file)| {
                let (reuse_key, artifact, clone_stats) = materialize_present_file(
                    config,
                    physical_artifacts,
                    retained_parses,
                    intake,
                    capability,
                    &manifest,
                    extractor,
                    chunker,
                    &repository_parse_identity,
                    parent,
                    file,
                    &captured_files,
                    control,
                )?;
                let key = u32::try_from(*key).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract("snapshot file key exceeds u32".to_owned())
                })?;
                let prior = parent.and_then(|parent| {
                    let descriptor = parent.file_segment(&file.logical_path)?;
                    let prior_file = parent
                        .generation()
                        .snapshot
                        .files
                        .get(descriptor.file_key as usize)?;
                    Some((parent.manifest(), prior_file, descriptor))
                });
                let mut identity_memory = ConstructionMemoryV1::new(worker_operation.clone())?;
                let encoded = match partitioned_codec::reusable_file_segment(
                    &manifest,
                    file,
                    &artifact,
                    key,
                    prior,
                    &mut |bytes| identity_memory.resize(bytes),
                )? {
                    Some(descriptor) => (descriptor, None),
                    None => {
                        let (descriptor, bytes) = partitioned_codec::encode_file_segment(
                            &manifest.generation_id,
                            &scope,
                            &artifact,
                            key,
                        )?;
                        (descriptor, Some(bytes))
                    }
                };
                Ok::<_, CodeIndexProductionErrorV1>((reuse_key, artifact, encoded, clone_stats))
            })?;
            for (reuse_key, artifact, (descriptor, bytes), clone_stats) in artifacts {
                clones_reused = clones_reused.saturating_add(clone_stats.reused);
                clones_computed = clones_computed.saturating_add(clone_stats.computed);
                lexical_page_source::checkpoint(control)?;
                if let Some(bytes) = bytes {
                    preparation.write_segment(SealedGenerationSegmentPublicationV1::File {
                        digest: &descriptor.segment_digest,
                        bytes: &bytes,
                    })?;
                }
                chunk_count = chunk_count.saturating_add(artifact.artifacts.chunks.chunks.len());
                for chunk in &artifact.artifacts.chunks.chunks {
                    chunk_policy =
                        chunk_policy.with_chunks_under(&chunk.sensitivity.policy_revision);
                }
                file_segments.push(descriptor);
                captured_files.remove(&artifact.extraction.file_occurrence_id);
                self.physical_artifacts.insert(reuse_key, &artifact);
                // Both cache authorities are weak: dropping this artifact is
                // the final owner of its full extraction/chunk payload.
                drop(artifact);
                memory.resize(extraction_base as u64)?;
            }
        }
        drop(captured_files);
        drop(present);
        drop(scope);
        let file_descriptor_bytes = resident_bytes::file_segment_descriptors_bytes(
            &file_segments,
            file_segments.capacity(),
        )
        .saturating_add(resident_bytes::chunk_policy_heap_bytes(&chunk_policy));
        memory.resize(file_descriptor_bytes as u64)?;
        tracing::trace!(
            phase = "extraction",
            files = file_segments.len(),
            rows = chunk_count,
            file_descriptor_bytes,
            charged_bytes = memory.charged_bytes,
            "cold build phase completed"
        );
        drop(extraction_span);
        #[cfg(any(test, feature = "test-helpers"))]
        hold_after_cold_extraction_for_test(control)?;
        let resolution_span = tracing::trace_span!("code_index.cold.resolution").entered();
        let resolution_base = memory.charged_bytes;
        let reader = preparation.segment_reader();
        let mut reuse_memory = ConstructionMemoryV1::new(memory.operation.clone())?;
        let reusable = partitioned_codec::ParentSegmentReuseRosterV1::new(
            parent.map(SealedParentGenerationV1::generation),
            &mut |bytes| reuse_memory.resize(bytes),
        )?;
        let resolved = sparse_successor::resolve_staged_generation(
            &manifest,
            snapshot,
            &file_segments,
            reader,
            control,
            &mut |facts| memory.resize(resolution_base.saturating_add(facts)),
            &mut |segment| {
                if reusable.contains(segment) {
                    Ok(())
                } else {
                    preparation.write_segment(segment).map_err(Into::into)
                }
            },
        )?;
        drop(reusable);
        drop(reuse_memory);
        tracing::trace!(
            phase = "resolution",
            charged_bytes = memory.charged_bytes,
            "cold build phase completed"
        );
        drop(resolution_span);
        // Resolution facts have dropped; its returned descriptors still live.
        let descriptor_bytes = file_descriptor_bytes.saturating_add(
            resident_bytes::staged_resolution_descriptors_bytes(&resolved),
        );
        memory.resize(descriptor_bytes as u64)?;
        lexical_page_source::checkpoint(control)?;
        let projection_span = tracing::trace_span!("code_index.cold.projection").entered();
        // Changed-chunk tuples are projection inputs, not resolution facts.
        // Decode one sealed file after resolution drops instead of retaining
        // the repository-wide tuple roster beside all binding facts.
        let mut rows = Vec::<(u32, ChangedCodeChunkV1)>::new();
        let mut rows_heap = 0_usize;
        let source = sealed_parent::StagedFileSourceV1::new(
            &manifest,
            snapshot,
            &file_segments,
            preparation.segment_reader(),
        )?;
        for descriptor in &file_segments {
            lexical_page_source::checkpoint(control)?;
            let artifact = source.decode_file(descriptor.file_key)?;
            let key = descriptor.file_key;
            let projection_file_base = descriptor_bytes
                .saturating_add(resolution_view::resolution_source_bytes(&artifact)?);
            let incoming = &artifact.artifacts.chunks.chunks;
            let next_heap = incoming.iter().fold(rows_heap, |bytes, chunk| {
                bytes
                    .saturating_add(chunk.id.as_str().len())
                    .saturating_add(chunk.content_digest.as_str().len())
            });
            let needed = rows.len().saturating_add(incoming.len());
            if needed > rows.capacity() {
                let capacity = rows.capacity().saturating_mul(2).max(needed);
                memory.resize(
                    projection_file_base
                        .saturating_add(next_heap)
                        .saturating_add(
                            rows.capacity()
                                .saturating_add(capacity)
                                .saturating_mul(std::mem::size_of::<(u32, ChangedCodeChunkV1)>()),
                        ) as u64,
                )?;
                rows.try_reserve_exact(capacity - rows.len())
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            }
            memory.resize(
                projection_file_base
                    .saturating_add(next_heap)
                    .saturating_add(
                        rows.capacity()
                            .saturating_mul(std::mem::size_of::<(u32, ChangedCodeChunkV1)>()),
                    ) as u64,
            )?;
            rows_heap = next_heap;
            for chunk in incoming {
                rows.push((
                    key,
                    ChangedCodeChunkV1 {
                        chunk_id: chunk.id.clone(),
                        prior_digest: None,
                        current_digest: Some(chunk.content_digest.clone()),
                    },
                ));
            }
            drop(artifact);
            memory.resize(
                descriptor_bytes.saturating_add(rows_heap).saturating_add(
                    rows.capacity()
                        .saturating_mul(std::mem::size_of::<(u32, ChangedCodeChunkV1)>()),
                ) as u64,
            )?;
        }
        drop(source);
        let row_tuple_bytes = rows_heap.saturating_add(
            rows.capacity()
                .saturating_mul(std::mem::size_of::<(u32, ChangedCodeChunkV1)>()),
        );
        tracing::trace!(
            phase = "projection",
            rows = rows.len(),
            descriptor_bytes,
            row_tuple_bytes,
            charged_bytes = memory.charged_bytes,
            "cold build phase started"
        );
        rows.sort_unstable_by(|left, right| left.1.chunk_id.cmp(&right.1.chunk_id));
        let count = rows.len();
        memory.resize(
            descriptor_bytes
                .saturating_add(row_tuple_bytes)
                .saturating_add(count.saturating_mul(
                    std::mem::size_of::<u32>() + std::mem::size_of::<ChangedCodeChunkV1>(),
                )) as u64,
        )?;
        let mut keys = Vec::with_capacity(count);
        let mut added_or_changed = Vec::with_capacity(count);
        for (key, row) in rows {
            keys.push(key);
            added_or_changed.push(row);
        }
        // The compact projection will own these headers in addition to its
        // row vector; source commitments own two more tagged digests.
        let projection_headers = manifest
            .generation_id
            .as_str()
            .len()
            // Changed-set manifest + reuse, request + publication, and the
            // two source-commitment digests each own one tagged SHA-256 string.
            .saturating_add(6 * 71)
            .saturating_add(target_projection_key.schema_revision.capacity())
            .saturating_add(target_projection_key.profile_digest.as_str().len())
            .saturating_add(resident_bytes::ARC_HEADER_BYTES)
            .saturating_add(std::mem::size_of::<ProjectionBatchRequestV1>())
            .saturating_add(std::mem::size_of::<ManifestDigest>())
            .saturating_add(std::mem::size_of::<
                Vec<(usize, tracedecay_domain::ContentDigest)>,
            >());
        let projection_base = descriptor_bytes
            .saturating_add(rows_heap)
            .saturating_add(projection_headers)
            .saturating_add(count.saturating_mul(
                std::mem::size_of::<u32>() + std::mem::size_of::<ChangedCodeChunkV1>(),
            ));
        memory.resize(projection_base as u64)?;
        let (reused_count, reused_digest) = memory.canonical(
            projection_base,
            partitioned_codec::canonical_object_workspace_bytes(0, 2),
            || {
                ChangedCodeChunkSetV1::seal_reused_partition(&[])
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
            },
        )?;
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: manifest.generation_id.clone(),
            manifest_digest: placeholder_digest(),
            added_or_changed,
            deleted: Vec::new(),
            reused_count,
            reused_digest,
        };
        let change_workspace = changed_set_hash_workspace(&changes)?;
        tracing::trace!(
            phase = "projection",
            retained_base_bytes = projection_base,
            canonical_change_workspace_bytes = change_workspace,
            "cold projection change workspace"
        );
        memory.canonical(projection_base, change_workspace, || {
            changes
                .seal()
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
        })?;
        manifest.source_commitments =
            Some(memory.canonical(projection_base, change_workspace, || {
                CodeGenerationSourceCommitmentsV1::from_changed_chunks(None, &changes)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
            })?);
        let seal_workspace = manifest_hash_workspace(&manifest)?;
        manifest.seal.expected_digest =
            memory.canonical(projection_base, seal_workspace, || {
                expected_seal_digest(&manifest)
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
            })?;
        let capability = emit_cold_capability(
            &mut memory,
            projection_base,
            intake,
            &manifest,
            snapshot,
            resolved.coverage,
        )?;
        let capability_bytes = resident_bytes::capability_heap_bytes(&capability);
        let projection_base = projection_base.saturating_add(capability_bytes);
        let request_workspace =
            change_workspace.saturating_add(partitioned_codec::canonical_object_workspace_bytes(
                partitioned_codec::encoded_json_rows_length(&target_projection_key)?,
                3,
            ));
        let request = memory.canonical(projection_base, request_workspace, || {
            projection_request(None, target_projection_key, changes)
        })?;
        // Publication hashes stream their receipt sequence. Only one owned
        // receipt and its object buffer coexist with the changed-set verifier.
        let transient_receipt = resident_bytes::projection_receipt_heap_bound(&request);
        memory.resize(projection_base.saturating_add(transient_receipt) as u64)?;
        let receipt_json = crate::receipts::ordered_changes(&request.changes).try_fold(
            0_usize,
            |largest, change| {
                partitioned_codec::encoded_json_rows_length(&crate::receipts::applied_receipt(
                    &request, change,
                ))
                .map(|bytes| largest.max(bytes))
            },
        )?;
        let publication_workspace = request_workspace
            .saturating_add(transient_receipt)
            .saturating_add(partitioned_codec::canonical_object_workspace_bytes(
                receipt_json,
                12,
            ));
        let projection = memory.canonical(projection_base, publication_workspace, || {
            project_for_publication(request).map_err(CodeIndexProductionErrorV1::Projection)
        })?;
        tracing::trace!(
            phase = "projection",
            charged_bytes = memory.charged_bytes,
            canonical_publication_workspace_bytes = publication_workspace,
            projection_bytes = resident_bytes::projection_heap_bytes(&projection),
            "cold build phase completed"
        );
        drop(projection_span);
        let evidence_span = tracing::trace_span!("code_index.cold.generation_evidence").entered();
        let keys_bytes = keys.capacity().saturating_mul(std::mem::size_of::<u32>());
        let evidence_base = descriptor_bytes
            .saturating_add(keys_bytes)
            .saturating_add(capability_bytes)
            .saturating_add(2 * 71)
            .saturating_add(resident_bytes::projection_heap_bytes(&projection));
        let rosters = projection_rows::FileChunkRostersV1::from_changes(
            &projection.request().changes.added_or_changed,
            &keys,
            evidence_base,
            &mut |bytes| memory.resize(bytes),
        )?;
        let roster_base = evidence_base.saturating_add(rosters.retained_bytes());
        let evidence = partitioned_codec::PartitionedGenerationEvidenceRefV1::new_charged(
            &projection,
            &rosters,
            None,
            roster_base,
            &mut |bytes| memory.resize(bytes),
        )?;
        let generation_evidence = partitioned_codec::write_generation_evidence_charged(
            &evidence,
            roster_base.saturating_add(evidence.retained_bytes()),
            &mut |bytes| memory.resize(bytes),
            &mut |segment| preparation.write_segment(segment).map_err(Into::into),
        )?;
        drop(evidence);
        drop(rosters);
        drop(keys);
        tracing::trace!(
            phase = "generation_evidence",
            charged_bytes = memory.charged_bytes,
            descriptor_bytes =
                resident_bytes::generation_evidence_descriptor_bytes(&generation_evidence),
            "cold build phase completed"
        );
        drop(evidence_span);
        let manifest_span = tracing::trace_span!("code_index.cold.manifest").entered();
        let manifest_base = descriptor_bytes
            .saturating_add(capability_bytes)
            .saturating_add(2 * 71)
            .saturating_add(resident_bytes::projection_heap_bytes(&projection))
            .saturating_add(resident_bytes::generation_evidence_descriptor_bytes(
                &generation_evidence,
            ));
        memory.resize(manifest_base as u64)?;
        let chunk_count = u64::try_from(projection.receipt_count()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("generation chunk count exceeds u64".to_owned())
        })?;
        let generation_ref = partitioned_codec::PartitionedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &manifest,
            snapshot,
            statistics: &resolved.statistics,
            chunk_count,
            chunk_policy: &chunk_policy,
            repository_parse_identity: &repository_parse_identity,
            ignored_source_admissions: ignored_source_roster.admissions(),
            ignored_source_admissions_digest: ignored_source_roster.digest(),
            file_segments: &file_segments,
            file_evidence: &resolved.file_evidence,
            coverage: resolved.coverage,
            capability: &capability,
            generation_evidence: &generation_evidence,
            code_graph_pages: &resolved.code_graph_pages,
            resolution_index: &resolved.resolution_index,
        };
        let manifest_bytes = partitioned_codec::seal_partitioned_manifest_charged(
            &generation_ref,
            manifest_base,
            &mut |bytes| memory.resize(bytes),
        )?;
        let mut string_scratch = partitioned_codec::encoded_json_rows_length(&manifest)?
            .max(partitioned_codec::encoded_json_rows_length(&capability)?);
        for file in &snapshot.files {
            string_scratch = string_scratch.max(partitioned_codec::encoded_json_rows_length(file)?);
        }
        for omitted in &snapshot.omitted_sources {
            string_scratch =
                string_scratch.max(partitioned_codec::encoded_json_rows_length(omitted)?);
        }
        for admission in ignored_source_roster.admissions() {
            string_scratch =
                string_scratch.max(partitioned_codec::encoded_json_rows_length(admission)?);
        }
        let metadata_workspace = metadata_hash_workspace(&manifest, snapshot)?;
        let decode_workspace = resident_bytes::partitioned_decode_workspace_bytes(
            &generation_ref,
            partitioned_codec::encoded_json_rows_length(&generation_ref)?,
            string_scratch,
        );
        // Vec -> Arc copies the bytes; both allocations coexist. Compact
        // verification then owns a decoded manifest and its validation scratch.
        let prepared_base = manifest_base
            .saturating_add(manifest_bytes.capacity())
            .saturating_add(manifest_bytes.len())
            .saturating_add(resident_bytes::ARC_HEADER_BYTES);
        let mut publication = memory.canonical(
            prepared_base.saturating_add(decode_workspace),
            metadata_workspace,
            || {
                CodeIndexSealedPublicationV1::from_prepared(
                    Arc::from(manifest_bytes),
                    projection,
                    Some(cold_reason),
                    (clones_reused, clones_computed, 0),
                )
            },
        )?;
        drop(generation_evidence);
        drop(file_segments);
        drop(resolved);
        drop(capability);
        drop(manifest);
        let policy_bytes = resident_bytes::chunk_policy_heap_bytes(&chunk_policy);
        drop(chunk_policy);
        memory
            .resize(resident_bytes::sealed_publication_bytes(&publication, policy_bytes) as u64)?;
        tracing::trace!(
            phase = "manifest",
            charged_bytes = memory.charged_bytes,
            chunk_count,
            manifest_bytes = publication.manifest_bytes().len(),
            "cold build phase completed"
        );
        drop(manifest_span);
        publication.construction_memory = Some(Arc::new(memory));
        Ok(publication)
    }
}

/// Park a cold build after it extracted every present file and before it
/// assembles and publishes the generation, while the file
/// `TRACEDECAY_TEST_HOLD_AFTER_COLD_EXTRACTION` names exists and the build is
/// not cancelled. Records `<hold>.entered` on arrival. Builds that run before
/// the test creates the file pass straight through.
///
/// Compiled only under `test-helpers`, so a default or `production` build
/// does not read the variable.
#[cfg(any(test, feature = "test-helpers"))]
fn hold_after_cold_extraction_for_test(
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<(), CodeIndexProductionErrorV1> {
    let Some(hold) = std::env::var_os("TRACEDECAY_TEST_HOLD_AFTER_COLD_EXTRACTION") else {
        return Ok(());
    };
    let hold = std::path::PathBuf::from(hold);
    if !hold.exists() {
        return Ok(());
    }
    let entered = std::path::PathBuf::from(format!("{}.entered", hold.display()));
    std::fs::write(&entered, b"cold-extraction").map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "could not record the test hold at {}: {error}",
            entered.display()
        ))
    })?;
    while hold.exists() && !control.is_cancelled() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

fn interruption_error(control: &dyn CodeIndexExecutionControlV1) -> CodeIndexProductionErrorV1 {
    if control.is_deadline_exceeded() {
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
    } else {
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    }
}

/// Materialize one present file for a cold build: extract captured bytes,
/// or carry the sealed parent file when the snapshot row is unchanged.
///
/// A watched refresh captures only the files the edit changed. When the
/// increment must resolve whole (Go method sets, moved name lookups), the
/// parent still authenticates every unchanged file, so those bytes are not
/// recaptured. A present file that is neither captured nor carryable is
/// [`CodeIndexInputErrorV1::MissingCapturedFile`].
#[allow(clippy::too_many_arguments)]
fn materialize_present_file(
    config: &CodeIndexProductionConfigV1,
    physical_artifacts: &SharedPhysicalCodeArtifactPoolV1,
    retained_parses: &SharedRetainedParsePool,
    intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
    capability: &SanitizedSnapshotCapabilityV1,
    manifest: &CodeGenerationManifestV1,
    extractor: &TreeSitterExtractor,
    chunker: &DeterministicCodeChunker,
    repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
    parent: Option<&SealedParentGenerationV1>,
    file: &SanitizedCodeFileV1,
    captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<
    (
        ManifestDigest,
        Arc<FileGenerationArtifactsV1>,
        ClonePayloadBuildStatsV1,
    ),
    CodeIndexProductionErrorV1,
> {
    if captured_files.contains_key(&file.file_occurrence_id) {
        return extract_file(
            config,
            physical_artifacts,
            retained_parses,
            false,
            intake,
            capability,
            manifest,
            extractor,
            chunker,
            repository_parse_identity,
            file,
            None,
            captured_files,
            control,
        );
    }
    let Some(parent) = parent else {
        return Err(CodeIndexInputErrorV1::MissingCapturedFile.into());
    };
    let Some(carried) =
        parent.carry_unchanged_file(file, &manifest.generation_id, &manifest.snapshot_digest)?
    else {
        return Err(CodeIndexInputErrorV1::MissingCapturedFile.into());
    };
    let language = file.language.as_ref().ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "present snapshot file has no declared language".to_owned(),
        )
    })?;
    let descriptor = intake.registry().descriptor(language).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "validated snapshot language has no descriptor".to_owned(),
        )
    })?;
    let sensitivity = carried
        .artifacts
        .chunks
        .chunks
        .first()
        .map_or(SensitivityLevelV1::Public, |chunk| chunk.sensitivity.level);
    let physical_reuse_key = physical_reuse_key(config, file, descriptor, sensitivity)?;
    let reused = u64::try_from(carried.artifacts.clone_bodies.len()).unwrap_or(u64::MAX);
    physical_artifacts.record_clone_payloads(reused, 0);
    Ok((
        physical_reuse_key,
        carried,
        ClonePayloadBuildStatsV1 {
            reused,
            computed: 0,
        },
    ))
}

/// Extract one file's generation artifacts, returning the physical reuse
/// key alongside them: callers record artifacts into the pool in canonical
/// snapshot order after the parallel sweep, and the key binds the same
/// inputs either way, so recomputing it per recording was pure waste.
#[allow(clippy::too_many_arguments)]
fn extract_file(
    config: &CodeIndexProductionConfigV1,
    physical_artifacts: &SharedPhysicalCodeArtifactPoolV1,
    retained_parses: &SharedRetainedParsePool,
    retain_parse: bool,
    intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
    capability: &SanitizedSnapshotCapabilityV1,
    manifest: &CodeGenerationManifestV1,
    extractor: &TreeSitterExtractor,
    chunker: &DeterministicCodeChunker,
    repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
    file: &SanitizedCodeFileV1,
    prior_clone_bodies: Option<&[CodeIndexCloneBodyV1]>,
    captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<
    (
        ManifestDigest,
        Arc<FileGenerationArtifactsV1>,
        ClonePayloadBuildStatsV1,
    ),
    CodeIndexProductionErrorV1,
> {
    crate::observe::measure_hot_loop!("code_index.materialize.file", {
        lexical_page_source::checkpoint(control)?;
        let captured = captured_files
            .get(&file.file_occurrence_id)
            .ok_or(CodeIndexInputErrorV1::MissingCapturedFile)?;
        let receipt_bound = intake
            .bind_file(
                capability,
                &config.project_id,
                ValidatedCodeFileV1 {
                    generation_id: manifest.generation_id.clone(),
                    file: file.clone(),
                    snapshot_digest: capability.snapshot().intake_digest.clone(),
                    sanitized_bytes: captured.sanitized_bytes.to_vec(),
                },
            )
            .map_err(CodeIndexProductionErrorV1::Intake)?;
        let language = file.language.as_ref().ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "present snapshot file has no declared language".to_owned(),
            )
        })?;
        let descriptor = intake.registry().descriptor(language).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "validated snapshot language has no descriptor".to_owned(),
            )
        })?;
        let physical_reuse_key =
            physical_reuse_key(config, file, descriptor, captured.sensitivity_level)?;
        if let Some(reused) = physical_artifacts.reuse(
            &physical_reuse_key,
            &receipt_bound,
            &descriptor.extractor_revision,
        ) {
            physical_artifacts.record_clone_payloads(
                u64::try_from(reused.artifacts.clone_bodies.len()).unwrap_or(u64::MAX),
                0,
            );
            lexical_page_source::checkpoint(control)?;
            let clone_stats = ClonePayloadBuildStatsV1 {
                reused: u64::try_from(reused.artifacts.clone_bodies.len()).unwrap_or(u64::MAX),
                computed: 0,
            };
            return Ok((physical_reuse_key, reused, clone_stats));
        }
        let snapshot = &capability.snapshot().snapshot;
        let parser = extractor
            .resolve_parser(receipt_bound.validated_file(), descriptor)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Extraction(ExtractionFailureV1::GrammarUnavailable {
                    language: descriptor.language.clone(),
                })
            })?;
        if crate::languages::canonical_language_id(parser.language_name())
            != descriptor.language.as_str()
        {
            return Err(CodeIndexProductionErrorV1::Extraction(
                ExtractionFailureV1::IncompatibleDescriptor {
                    detail: format!(
                        "descriptor {} resolved to a {} parser",
                        descriptor.language,
                        parser.language_name()
                    ),
                },
            ));
        }
        let cancellation = ExtractionControlBridge { control };
        let extraction = match parse_for_indexing(
            retained_parses,
            retain_parse,
            ParseDocumentIdentity::Repository {
                project_id: config.project_id.clone(),
                repository_id: snapshot.repository.clone(),
                worktree_id: snapshot.worktree.clone(),
                reference: snapshot.reference.clone(),
                commit: snapshot.source_revision.clone(),
                tree: repository_parse_identity.tree.clone(),
                dirty: repository_parse_identity.dirty,
                logical_path: file.logical_path.clone(),
            },
            file,
            captured,
            parser,
            &descriptor.extractor_revision,
            control,
        ) {
            Ok((parse_artifacts, parsed_len)) => {
                lexical_page_source::checkpoint(control)?;
                extractor
                    .extract_preparsed(
                        &receipt_bound,
                        descriptor,
                        parse_artifacts,
                        parsed_len,
                        &cancellation,
                    )
                    .map_err(|error| match error {
                        ExtractionFailureV1::Cancelled | ExtractionFailureV1::TimedOut => {
                            interruption_error(control)
                        }
                        error => CodeIndexProductionErrorV1::Extraction(error),
                    })?
            }
            // A parse quantum is scheduling state, never evidence that the
            // source is unsupported. Only the enclosing operation can stop
            // admitted continuation, and it must not publish partial identity.
            Err(error @ CodeIndexProductionErrorV1::RetainedParse(ParseError::TimedOut { .. })) => {
                lexical_page_source::checkpoint(control)?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        lexical_page_source::checkpoint(control)?;
        let (artifacts, exact_authority, clone_stats) = chunker
            .index_file_with_authority_from_extraction_reusing(
                &receipt_bound,
                &extraction,
                descriptor,
                captured.sensitivity_level,
                &cancellation,
                prior_clone_bodies,
            )
            .map_err(|error| match error {
                ChunkingFailureV1::Cancelled => interruption_error(control),
                error => CodeIndexProductionErrorV1::Chunk(error),
            })?;
        physical_artifacts.record_clone_payloads(clone_stats.reused, clone_stats.computed);
        lexical_page_source::checkpoint(control)?;
        let (authority, extraction, _) = extraction.into_parts();
        let artifact = Arc::new(FileGenerationArtifactsV1 {
            authority,
            extraction,
            artifacts,
            exact_authority,
        });
        Ok((physical_reuse_key, artifact, clone_stats))
    })
}

fn physical_reuse_key(
    config: &CodeIndexProductionConfigV1,
    file: &SanitizedCodeFileV1,
    descriptor: &tracedecay_domain::LanguageDescriptorV1,
    sensitivity_level: SensitivityLevelV1,
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    crate::observe::measure_hot_loop!("code_index.materialize.reuse_key", {
        canonical_sha256(&(
            PHYSICAL_CODE_ARTIFACT_REUSE_DIGEST_DOMAIN,
            &config.project_id,
            &config.repository,
            // The file occurrence id is worktree-local identity and must
            // stay out of the byte-reuse key: linked worktrees share
            // physical parse/chunk artifacts for identical content, and
            // `rematerialize_for_file` rebinds the shared artifact onto
            // each worktree's own occurrence after a hit.
            &file.logical_path,
            &file.content_digest,
            descriptor,
            &config.sanitizer_revision,
            &config.policy_revision,
            &config.chunker_revision,
            &config.privacy_domain,
            config.privacy_key_epoch,
            sensitivity_level,
        ))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    })
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;

#[cfg(test)]
#[path = "sparse_increment_tests.rs"]
mod sparse_increment_tests;

#[cfg(test)]
#[path = "sparse_differential_tests.rs"]
mod sparse_differential_tests;

#[cfg(test)]
#[path = "file_evidence_tests.rs"]
mod file_evidence_tests;
