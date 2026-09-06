//! Production composition for immutable code-index generation publication.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_code_extraction::incremental::ParseError;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeGenerationManifestV1,
    CodeIndexCapabilityManifestV1, ComponentVersion, CoverageSummaryV1, FileOccurrenceId,
    GenerationTestAttributionV1, ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1,
    ProviderEvaluationStateV1, RefId, RepositoryId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
    TestAttributionEvidenceClassV1, UtcMicros, ValidatedCodeFileV1, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphGenerationManifest, GraphProjectionIdentity, GraphProjectorRevision,
};

use super::{
    capabilities::{BaseCapabilityEmitter, CapabilityEmissionErrorV1, CodeIndexCapabilityEmitter},
    chunks::{
        ChunkingFailureV1, CodeFileIndexArtifactsV1, CodeIndexEdgeAbstentionV1,
        CodeIndexImportEvidenceV1, DeterministicCodeChunker, ExactExtractionAuthorityV1,
        ExtractionAdmittedCodeSearchChunkV1, content_digest,
    },
    extract::{ExtractionCancellation, TreeSitterExtractor, rebind_extraction_batch},
    generations::{FileExtractionActionV1, GenerationPlanner, GenerationPlanningErrorV1},
    incremental::{
        ChunkIncrementErrorV1, GenerationChunkManifestV1, materialize_generation_increment,
        plan_chunk_increment,
    },
    intake::{
        CodeIndexIntake, ReceiptBoundCodeFileAuthorityV1, ReceiptBoundCodeFileV1,
        SanitizedCodeIntake, SanitizedSnapshotCapabilityV1,
    },
    languages::{LanguageRegistry, StaticLanguageRegistry},
    lineage::{GenerationSymbolIndexV1, LineageResolutionErrorV1, SymbolLineageResolver},
    projection::{
        CodeChunkProjectionSink, ProjectionPublicationErrorV1, ProjectionPublicationHandoffV1,
        expected_request_digest, project_for_publication,
    },
    provider::{
        GenerationProviderCoverageV1, GenerationProviderReadV1,
        GenerationTestAttributionJoinReadPort,
    },
    retained_parse::{RetainedParsePoolStats, SharedRetainedParsePool},
    test_attribution::{
        GenerationTestJoinV1, TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1,
        TestAttributionWatermarkV1,
    },
};

mod canonical_json;
mod helpers;
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
    VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageBatchBoundsV1,
    VerifiedSealedLexicalPageBatchReadV1, VerifiedSealedLexicalPageReadV1,
    VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1, VerifiedSealedLexicalSymbolDisplayV1,
    VerifiedSealedTextGenerationMetadataV1,
};
mod partitioned_codec;
pub use partitioned_codec::{
    SealedGenerationSegmentIdentityV1, SealedGenerationSegmentPublicationV1,
    SealedGenerationSegmentReadV1,
};
mod sealed_codec;
pub use sealed_codec::{
    MAX_SEALED_CODE_GENERATION_BYTES_V1, MINIMUM_SEALED_GENERATION_FORMAT_REVISION,
    SEALED_GENERATION_FORMAT_REVISION_V1, sealed_generation_format_revision_is_compatible,
    sealed_generation_payload_digest, superseded_sealed_generation_revision,
};

/// Current daemon chunker identity shared by production indexing and native
/// semantic evaluation fixtures. Historical revisions remain decodable but
/// must never be emitted as current activation evidence.
pub const DAEMON_CODE_INDEX_CHUNKER_REVISION: &str = "chunker.daemon.v3";

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
    SanitizerRevision,
    PolicyRevision,
    MixedPolicyRevisions,
    ChunkerRevision,
    PrivacyDomain,
    PrivacyKeyEpoch,
}

impl CodeIndexGenerationIncompatibilityV1 {
    #[hotpath::skip]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
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

    pub fn for_branch_stack_node(node: &tracedecay_domain::BranchStackNodeV1) -> Self {
        Self {
            repository: node.repository_id.clone(),
            reference: Some(node.reference.clone()),
            worktree: node.worktree_id.clone(),
        }
    }

    /// Whether two scopes name the same physical checkout.
    ///
    /// Repository and worktree are checkout identity: a generation sealed
    /// under either of them differing belongs to another checkout and may
    /// never be adopted or served for this one. `reference` is deliberately
    /// excluded — it is the branch label HEAD happens to carry, and it moves
    /// under a fixed worktree on every ordinary commit, branch switch, or
    /// rebase — so serving gates that need only checkout identity keep
    /// admitting the checkout's own generations across a label move. Slot
    /// dispatch is stricter: [`CodeIndexProductionOwnerV1::active_generation`]
    /// demands the complete scope, label included, because branch and worktree
    /// generations stay independently active inside one shared repository
    /// store.
    #[must_use]
    pub fn identifies_same_checkout(&self, other: &Self) -> bool {
        self.repository == other.repository && self.worktree == other.worktree
    }

    fn validate(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
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
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1>;

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1>;
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

enum IncrementFileMaterializationV1 {
    CarryForward(Arc<FileGenerationArtifactsV1>),
    ReExtracted {
        reuse_key: ManifestDigest,
        artifact: Arc<FileGenerationArtifactsV1>,
        fallback: bool,
    },
    Deleted,
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
/// batched form this does not abandon later files after a failure — the
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
    F: Fn(&T, &crate::hotpath_observe::WorkerBusyGuard) -> Result<R, E> + Sync,
{
    let queue = crate::hotpath_observe::PendingWorkQueue::new(items.len());
    crate::hotpath_observe::record_files(items.len());
    // Always enter the indexing pool, even when the width is 1: the pool is
    // what keeps the nested chunk-level fan-out inside the reservation
    // instead of spilling onto rayon's global (all-cores) pool.
    crate::parallelism::install(|| {
        let run = |(index, item): (usize, &T)| -> Result<R, E> {
            crate::parallelism::with_background_cpu_permit(|| {
                let worker = queue.start_worker();
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(item, &worker)))
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
        worker: &crate::hotpath_observe::WorkerBusyGuard,
    ) -> Option<Arc<FileGenerationArtifactsV1>> {
        crate::hotpath_observe::measure_hot_loop!("code_index.artifact_pool.reuse", {
            let artifact = {
                let _coordination = worker.pool_coordination();
                upgrade_weak_under_lock(&self.state, |state| state.artifacts.get(key).cloned())
            }?;
            let rebound = Arc::new(artifact.rematerialize_for_file(file).ok()?);
            {
                let _coordination = worker.pool_coordination();
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
        crate::hotpath_observe::measure_hot_loop!("code_index.artifact_pool.insert", {
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

    pub fn stats(&self) -> PhysicalCodeArtifactPoolStatsV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PhysicalCodeArtifactPoolStatsV1 {
            inserted: state.inserted,
            reused: state.reused,
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
    fn rematerialize_for_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
    ) -> Result<Self, ChunkingFailureV1> {
        crate::hotpath_observe::measure_hot_loop!("code_index.artifact_pool.rematerialize", {
            let target = file.validated_file();
            let artifacts = self.artifacts.rematerialize_for_generation(
                target.generation_id.clone(),
                target.file.file_occurrence_id.clone(),
            )?;
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

    fn rematerialize_carried_forward(
        &self,
        config: &CodeIndexProductionConfigV1,
        scope: &CodeIndexGenerationScopeV1,
        generation_id: &CodeGenerationId,
        file: &SanitizedCodeFileV1,
    ) -> Result<Self, ChunkingFailureV1> {
        if self.authority.project_id != config.project_id
            || self.authority.repository_id != config.repository
            || self.authority.worktree_id != scope.worktree
            || self.authority.reference != scope.reference
            || self.authority.logical_path != file.logical_path
            || self.authority.content_digest != file.content_digest
            || self.extraction.content_digest != file.content_digest
            || self.extraction.file_occurrence_id != file.file_occurrence_id
        {
            return Err(ChunkingFailureV1::GenerationMismatch);
        }
        let artifacts = self
            .artifacts
            .rematerialize_for_generation(generation_id.clone(), file.file_occurrence_id.clone())?;
        let exact_authority = self
            .exact_authority
            .rematerialize_for_generation(&self.artifacts.chunks, &artifacts.chunks)?;
        let mut extraction = self.extraction.clone();
        extraction.generation_id = generation_id.clone();
        extraction.file_occurrence_id = file.file_occurrence_id.clone();
        Ok(Self {
            authority: self.authority.clone(),
            extraction,
            artifacts,
            exact_authority,
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
    chunks: GenerationChunkManifestV1,
    symbols: GenerationSymbolIndexV1,
    lineage: Vec<SymbolLineageCandidateV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
    edges: Vec<CanonicalRelationEdgeV1>,
    edge_abstentions: Vec<CodeIndexEdgeAbstentionV1>,
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
    /// Amortized test-attribution join. Query admission rebuilds this authority
    /// per call even when the generation is unchanged; the traversal and its
    /// evidence digest are a pure function of the immutable generation. Only
    /// success is cached.
    attribution: OnceLock<PublishedGenerationTestAttributionAuthorityV1>,
    /// Amortized chunk policy-revision census. Owner-compatibility dispatch
    /// needs the one policy revision the chunks were sealed under; scanning
    /// every chunk on each `active_generation` call re-derived a value that is
    /// a pure function of the immutable generation.
    chunk_policy: OnceLock<ChunkPolicyRevisionSummaryV1>,
    /// Reclaimable code-graph publication manifest. Concurrent seat retries
    /// share a complete build while a publication caller owns it, but the
    /// generation does not pin the full entity/relation projection after the
    /// durable graph has consumed it. The key remains first-success-wins so a
    /// foreign projection identity can never replace the canonical memo.
    graph_manifest: OnceLock<Arc<Mutex<CodeGraphManifestMemoV1>>>,
}

/// One successfully built code-graph publication manifest, pinned to the
/// exact projection identity and projector revision it was derived under. A
/// lookup under any other identity is a memo miss, never an aliased manifest.
#[derive(Clone, Debug)]
struct CodeGraphManifestMemoV1 {
    projection: GraphProjectionIdentity,
    projector_revision: GraphProjectorRevision,
    manifest: Weak<GraphGenerationManifest>,
}

/// The chunk policy-revision census of one immutable generation: no chunks at
/// all, one uniform revision, or disagreeing revisions (which no owner
/// configuration can ever be compatible with).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ChunkPolicyRevisionSummaryV1 {
    Empty,
    Uniform(PolicyRevisionId),
    Mixed,
}

impl CodeIndexPublishedGenerationV1 {
    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.manifest
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.snapshot
    }

    /// The exact generation scope this generation was sealed under:
    /// repository, sealed branch label, and worktree.
    ///
    /// This — never a filesystem path and never the generation id — is the
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
    #[hotpath::measure(label = "code_index.generation.compatibility")]
    pub fn compatibility_with(
        &self,
        config: &CodeIndexProductionConfigV1,
    ) -> CodeIndexGenerationCompatibilityV1 {
        let mut incompatibilities = BTreeSet::new();
        if self.manifest.project_id != config.project_id {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::Project);
        }
        if self.manifest.sanitizer_revision != config.sanitizer_revision {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::SanitizerRevision);
        }
        match self.chunk_policy_summary() {
            ChunkPolicyRevisionSummaryV1::Empty => {}
            ChunkPolicyRevisionSummaryV1::Uniform(revision)
                if *revision != config.policy_revision =>
            {
                incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::PolicyRevision);
            }
            ChunkPolicyRevisionSummaryV1::Mixed => {
                incompatibilities
                    .insert(CodeIndexGenerationIncompatibilityV1::MixedPolicyRevisions);
            }
            ChunkPolicyRevisionSummaryV1::Uniform(_) => {}
        }
        if self.manifest.chunker_revision != config.chunker_revision {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::ChunkerRevision);
        }
        if self.manifest.privacy_domain != config.privacy_domain {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::PrivacyDomain);
        }
        if self.manifest.privacy_key_epoch != config.privacy_key_epoch {
            incompatibilities.insert(CodeIndexGenerationIncompatibilityV1::PrivacyKeyEpoch);
        }
        CodeIndexGenerationCompatibilityV1 { incompatibilities }
    }

    /// Build the production generation-bound affected-test authority.
    ///
    /// Test candidates are deliberately conservative: each callable symbol in
    /// a test-path file covers itself and every canonical graph occurrence
    /// reachable from it. Missing graph edges remain partial coverage rather
    /// than being upgraded into complete evidence.
    pub fn test_attribution_authority(
        &self,
    ) -> Result<PublishedGenerationTestAttributionAuthorityV1, CodeIndexProductionErrorV1> {
        if let Some(attribution) = self.attribution.get() {
            return Ok(attribution.clone());
        }
        let authority = self.build_test_attribution_authority()?;
        let _ = self.attribution.set(authority.clone());
        Ok(authority)
    }

    fn build_test_attribution_authority(
        &self,
    ) -> Result<PublishedGenerationTestAttributionAuthorityV1, CodeIndexProductionErrorV1> {
        let mut file_by_occurrence = BTreeMap::new();
        for file in &self.snapshot.files {
            file_by_occurrence.insert(
                file.file_occurrence_id.clone(),
                (file.logical_path.as_str(), file.content_digest.clone()),
            );
        }

        let mut occurrence_files: BTreeMap<
            SymbolOccurrenceId,
            (FileOccurrenceId, tracedecay_domain::ContentDigest),
        > = BTreeMap::new();
        for chunk in self.chunks.chunks() {
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
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((
                        chunk.anchor.file_occurrence_id.clone(),
                        content_digest.clone(),
                    ));
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get().0 != chunk.anchor.file_occurrence_id =>
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "test attribution occurrence crosses snapshot files".to_owned(),
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }

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
            .map(|symbol| symbol.occurrence.clone())
            .collect::<BTreeSet<_>>();
        let test_occurrences = occurrence_files
            .iter()
            .filter_map(|(occurrence, (file, _))| {
                callable_occurrences.contains(occurrence).then_some(())?;
                file_by_occurrence
                    .get(file)
                    .filter(|(path, _)| crate::is_test_file(path))
                    .map(|_| occurrence.clone())
            })
            .collect::<Vec<_>>();
        let mut outgoing: BTreeMap<SymbolOccurrenceId, Vec<SymbolOccurrenceId>> = BTreeMap::new();
        for edge in &self.edges {
            if occurrence_files.contains_key(&edge.from_occurrence)
                && occurrence_files.contains_key(&edge.to_occurrence)
            {
                outgoing
                    .entry(edge.from_occurrence.clone())
                    .or_default()
                    .push(edge.to_occurrence.clone());
            }
        }
        for destinations in outgoing.values_mut() {
            destinations.sort();
            destinations.dedup();
        }

        let attribution_revision =
            ComponentVersion::new("code-index.test-attribution.conservative.v1")
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let mut attributions = Vec::with_capacity(test_occurrences.len());
        for test_occurrence in test_occurrences {
            let mut covered = BTreeSet::from([test_occurrence.clone()]);
            let mut pending = VecDeque::from([test_occurrence.clone()]);
            while let Some(occurrence) = pending.pop_front() {
                for destination in outgoing.get(&occurrence).into_iter().flatten() {
                    if covered.insert(destination.clone()) {
                        pending.push_back(destination.clone());
                    }
                }
            }
            attributions.push(GenerationTestAttributionV1 {
                generation_id: self.manifest.generation_id.clone(),
                source_revision: self.snapshot.source_revision.clone(),
                test_occurrence,
                covered_occurrences: covered.into_iter().collect(),
                evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
                attribution_revision: attribution_revision.clone(),
            });
        }

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
        let mut watermark = TestAttributionWatermarkV1 {
            generation_id: self.manifest.generation_id.clone(),
            snapshot_digest: self.manifest.snapshot_digest.clone(),
            content_identity: self.snapshot.content_identity.clone(),
            source_revision: self.snapshot.source_revision.clone(),
            attribution_revision,
            evidence_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
            coverage: input_coverage,
        };
        watermark.evidence_digest = watermark
            .recompute_evidence_digest(&attributions, &occurrences)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let snapshot = ValidatedCodeSnapshotV1 {
            snapshot: self.snapshot.clone(),
            intake_digest: self.manifest.snapshot_digest.clone(),
            validated_at: self.manifest.seal.sealed_at,
        };
        let join = GenerationTestJoinV1::join(
            &self.manifest,
            &snapshot,
            &attributions,
            &occurrences,
            &watermark,
        )
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let eligible = attributions.len() as u64;
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
        Ok(PublishedGenerationTestAttributionAuthorityV1 {
            generation_id: self.manifest.generation_id.clone(),
            read,
        })
    }

    /// The memoized code-graph publication manifest for exactly this
    /// projection identity and projector revision, if a prior complete build
    /// recorded one. A key mismatch is a miss, never a substituted manifest.
    pub(crate) fn memoized_graph_manifest(
        &self,
        projection: &GraphProjectionIdentity,
        projector_revision: &GraphProjectorRevision,
    ) -> Option<Arc<GraphGenerationManifest>> {
        let memo = self.graph_manifest.get()?;
        let memo = match memo.lock() {
            Ok(memo) => memo,
            Err(poisoned) => poisoned.into_inner(),
        };
        (memo.projection == *projection && memo.projector_revision == *projector_revision)
            .then(|| memo.manifest.upgrade())
            .flatten()
    }

    /// Record one complete, successfully built code-graph publication
    /// manifest. First success wins; the generation is immutable, so any
    /// competing build under the same key produced an identical manifest.
    pub(crate) fn memoize_graph_manifest(
        &self,
        projection: GraphProjectionIdentity,
        projector_revision: GraphProjectorRevision,
        manifest: Arc<GraphGenerationManifest>,
    ) {
        let memo = self.graph_manifest.get_or_init(|| {
            Arc::new(Mutex::new(CodeGraphManifestMemoV1 {
                projection: projection.clone(),
                projector_revision: projector_revision.clone(),
                manifest: Weak::new(),
            }))
        });
        let mut memo = match memo.lock() {
            Ok(memo) => memo,
            Err(poisoned) => poisoned.into_inner(),
        };
        if memo.projection == projection && memo.projector_revision == projector_revision {
            memo.manifest = Arc::downgrade(&manifest);
        }
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
    /// answer. It is fail-closed because only success is memoized — a
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

        let mut files = self.files.iter().map(Arc::as_ref).collect::<Vec<_>>();
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
        hotpath::measure_block!(
            "code_index.collect.validate_files",
            collect_bounded_ordered(&files, |file, _worker| {
                file.artifacts
                    .validate()
                    .map_err(CodeIndexProductionErrorV1::Chunk)?;
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
                    || file.authority.worktree_id != self.snapshot.worktree
                    || file.authority.reference != self.snapshot.reference
                    || occurrence.is_none_or(|occurrence| {
                        occurrence.logical_path != file.authority.logical_path
                            || occurrence.content_digest != file.authority.content_digest
                    })
                    || file.extraction.content_digest != file.authority.content_digest
                    || file.extraction.generation_id != self.manifest.generation_id
                    || file.extraction.file_occurrence_id
                        != file.artifacts.chunks.document.file_occurrence_id
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
        )?;
        hotpath::measure_block!("code_index.collect.validate_aggregates", {
            validate_import_evidence(&files, &self.imports)?;
            let mut chunks = files
                .iter()
                .flat_map(|file| file.artifacts.chunks.chunks.iter())
                .collect::<Vec<_>>();
            chunks.sort_by(|left, right| left.id.cmp(&right.id));
            let chunks_match = chunks.len() == self.chunks.chunks().len()
                && chunks
                    .iter()
                    .zip(self.chunks.chunks())
                    .all(|(left, right)| *left == right);
            let mut symbols = files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter())
                .collect::<Vec<_>>();
            symbols.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
            let symbols_match = symbols.len() == self.symbols.symbols.len()
                && symbols
                    .iter()
                    .zip(&self.symbols.symbols)
                    .all(|(left, right)| *left == right);
            if !chunks_match || !symbols_match {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "published generation does not match file artifacts".to_owned(),
                ));
            }
            // Re-derive the generation's edge evidence (per-file edges plus
            // the seal-time cross-file resolution) with the same authority
            // that sealed it, so a published generation whose derivation and
            // stored evidence disagree is refused.
            let (edges, _) = collect_edge_evidence(&files);
            let edges_match = edges.len() == self.edges.len()
                && edges
                    .iter()
                    .zip(&self.edges)
                    .all(|(left, right)| left == right);
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
            if !edges_match || !abstentions_match {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "published graph evidence does not match file artifacts".to_owned(),
                ));
            }
            Ok::<_, CodeIndexProductionErrorV1>(())
        })
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
    #[error("code-index contract failed: {0}")]
    Contract(String),
    #[error("code-index parallel worker runtime failed: {0}")]
    Parallelism(#[from] crate::parallelism::CodeIndexParallelismErrorV1),
}

/// Slot lookup for one publish attempt.
///
/// `reusable` is the generation increment planning may adopt. `cas_incumbent`
/// is the compare-and-swap expected token: the same id when reuse is legal,
/// or the prior-label incumbent after a same-checkout label move that must
/// rebuild while still replacing the worktree slot atomically.
struct ActiveGenerationLookupV1 {
    reusable: Option<CodeIndexPublishedGenerationV1>,
    cas_incumbent: Option<CodeGenerationId>,
}

/// Production owner for one repository and one atomic publication authority.
pub struct CodeIndexProductionOwnerV1<P, S> {
    config: CodeIndexProductionConfigV1,
    publication: P,
    projection: S,
    physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    retained_parses: SharedRetainedParsePool,
}

impl<P, S> CodeIndexProductionOwnerV1<P, S>
where
    P: CodeIndexAtomicPublicationPort,
    S: CodeChunkProjectionSink,
{
    pub fn new(
        config: CodeIndexProductionConfigV1,
        publication: P,
        projection: S,
    ) -> Result<Self, CodeIndexProductionOpenErrorV1> {
        config.validate()?;
        Ok(Self {
            config,
            publication,
            projection,
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

    pub fn retained_parse_stats(&self) -> RetainedParsePoolStats {
        self.retained_parses.stats()
    }

    /// Load the currently reusable immutable generation. A restart therefore
    /// resumes from the publication authority rather than mutable worker state.
    ///
    /// Reuse is full-scope exact: the loaded generation must have been sealed
    /// under the requested repository, reference, and worktree. A same-checkout
    /// reference label move is a rebuild (`Ok(None)`), not reuse — the
    /// worktree-scoped slot still holds the prior label's incumbent, which
    /// [`Self::build_and_publish`] keeps as the compare-and-swap expected
    /// token. A publication authority that answers a scope with a generation
    /// sealed for a *foreign checkout* has broken its slot partition — or the
    /// caller's checkout identity resolution regressed, e.g. a repository
    /// misclassified as not-a-git-path. That is the terminal
    /// [`CodeIndexPublicationStoreErrorV1::CorruptionResetRequired`] state:
    /// the foreign generation is never adopted, never config-checked, and the
    /// refusal is a reset journey, not a transient error to retry on a
    /// cadence.
    pub fn active_generation(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        Ok(self.lookup_active_generation(scope)?.reusable)
    }

    #[hotpath::measure(label = "code_index.build.active_generation")]
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
        let sealed_scope = active.sealed_scope();
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
                    cas_incumbent: Some(active.manifest.generation_id.clone()),
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
        active.validate()?;
        let compatibility = active.compatibility_with(&self.config);
        if !compatibility.is_reusable() {
            return Ok(ActiveGenerationLookupV1 {
                reusable: None,
                cas_incumbent: Some(active.manifest.generation_id.clone()),
            });
        }
        let cas_incumbent = Some(active.manifest.generation_id.clone());
        Ok(ActiveGenerationLookupV1 {
            reusable: Some(active),
            cas_incumbent,
        })
    }

    /// Build one complete generation and atomically publish it only after
    /// intake, parser evidence, lineage, exact admission, projection receipt,
    /// and capability validation have all succeeded.
    #[hotpath::measure(label = "code_index.build.and_publish")]
    pub fn build_and_publish(
        &mut self,
        request: CodeIndexBuildRequestV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Arc<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        let started = crate::hotpath_observe::start_build_to_queryable();
        crate::hotpath_observe::record_generation_state("building");
        crate::hotpath_observe::record_rebuild_state("unknown");
        Self::checkpoint(control)?;
        let ignored_source_roster = IgnoredSourceRosterV1::admit(
            &request.snapshot,
            &request.repository_parse_identity,
            &request.ignored_source_admissions,
        )?;
        let scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
        let lookup = self.lookup_active_generation(&scope)?;
        let active = lookup.reusable;
        Self::checkpoint(control)?;

        let intake = self.intake_at(request.sealed_at, registry_for_snapshot(&request.snapshot)?);
        let capability = intake
            .admit(request.snapshot.clone())
            .map_err(CodeIndexProductionErrorV1::Intake)?;
        let validated = capability.snapshot().clone();
        let captured_files = captured_files(&validated.snapshot, request.captured_files)?;
        #[cfg(feature = "hotpath")]
        {
            crate::hotpath_observe::record_files(captured_files.len());
            crate::hotpath_observe::record_source_bytes(
                captured_files
                    .values()
                    .map(|file| file.sanitized_bytes.len() as u64)
                    .fold(0_u64, u64::saturating_add),
            );
        }
        Self::checkpoint(control)?;

        let planner = GenerationPlanner::new(
            self.config.project_id.clone(),
            self.config.repository.clone(),
            registry_for_snapshot(&validated.snapshot)?,
            self.config.chunker_revision.clone(),
            self.config.privacy_domain.clone(),
            self.config.privacy_key_epoch,
        );
        let (manifest, increment) = match active.as_ref() {
            Some(active) => {
                let plan = planner
                    .plan_increment_with_invalidation(
                        &active.manifest,
                        &active.snapshot,
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
                        Some(&active.manifest),
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
        Self::checkpoint(control)?;

        let parser_registry = Arc::new(tracedecay_code_extraction::LanguageRegistry::new());
        let extractor = TreeSitterExtractor::from_shared_registry(Arc::clone(&parser_registry));
        let chunker = DeterministicCodeChunker::from_shared_registry(
            manifest.generation_id.clone(),
            self.config.repository.clone(),
            self.config.sanitizer_revision.clone(),
            self.config.policy_revision.clone(),
            self.config.chunker_revision.clone(),
            parser_registry,
        );
        crate::hotpath_observe::record_rebuild_state(match increment.as_ref() {
            Some(plan) if plan.is_full_rebuild() => "rebuild",
            Some(_) => "increment",
            None => "full",
        });
        crate::hotpath_observe::record_generation_state(if active.is_some() {
            "resume"
        } else {
            "initial"
        });
        let staged = match (active.as_ref(), increment.as_ref()) {
            (Some(active), Some(increment)) => self.materialize_increment(
                &intake,
                &capability,
                &manifest,
                &extractor,
                &chunker,
                &request.repository_parse_identity,
                active,
                increment,
                &captured_files,
                control,
            )?,
            (None, None) => self.materialize_full(
                &intake,
                &capability,
                &manifest,
                &extractor,
                &chunker,
                &request.repository_parse_identity,
                &validated.snapshot,
                &captured_files,
                control,
            )?,
            _ => {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "active generation and increment plan disagree".to_owned(),
                ));
            }
        };
        Self::checkpoint(control)?;

        let candidate = hotpath::measure_block!("code_index.build.assemble", {
            let coverage = coverage_summary(&validated.snapshot, &staged.files);
            let capability = BaseCapabilityEmitter::new(
                registry_for_snapshot(&validated.snapshot)?,
                coverage,
                validated.snapshot.sanitization_receipts.clone(),
            )
            .emit(&manifest)
            .map_err(CodeIndexProductionErrorV1::Capability)?;
            let changes =
                plan_chunk_increment(active.as_ref().map(|active| &active.chunks), &staged.chunks)
                    .map_err(CodeIndexProductionErrorV1::Increment)?;
            let projection_request = projection_request(
                active.as_ref(),
                increment.as_ref(),
                request.target_projection_key,
                changes,
            )?;
            Self::checkpoint(control)?;
            let projection = project_for_publication(&mut self.projection, projection_request)
                .map_err(CodeIndexProductionErrorV1::Projection)?;
            Self::checkpoint(control)?;

            let imports = derive_import_evidence(&staged.files);
            let (edges, edge_abstentions) = collect_edge_evidence(&staged.files);
            let candidate = CodeIndexPublishedGenerationV1 {
                manifest,
                snapshot: validated.snapshot,
                repository_parse_identity: request.repository_parse_identity.clone(),
                ignored_source_roster,
                files: staged.files,
                chunks: staged.chunks,
                symbols: staged.symbols,
                lineage: staged.lineage,
                imports,
                edges,
                edge_abstentions,
                coverage,
                capability,
                projection,
                validated: OnceLock::new(),
                admitted: OnceLock::new(),
                attribution: OnceLock::new(),
                chunk_policy: OnceLock::new(),
                graph_manifest: OnceLock::new(),
            };
            candidate.validate()?;
            Ok::<_, CodeIndexProductionErrorV1>(candidate)
        })?;
        #[cfg(feature = "hotpath")]
        if let Ok(statistics) = candidate.generation_statistics() {
            crate::hotpath_observe::record_source_bytes(statistics.source_total_bytes);
            crate::hotpath_observe::record_symbols(statistics.symbol_count);
            crate::hotpath_observe::record_relations(statistics.edge_count);
            crate::hotpath_observe::record_files(candidate.files.len());
        }

        let expected = lookup.cas_incumbent;
        // Shared rather than cloned: the publication store caches the same
        // immutable generation the caller receives.
        let candidate = Arc::new(candidate);
        hotpath::measure_block!("code_index.build.publish", {
            self.publication
                .publish_atomically(&scope, expected.as_ref(), Arc::clone(&candidate))
        })?;
        crate::hotpath_observe::record_generation_state("queryable");
        crate::hotpath_observe::record_build_to_queryable(started);
        Ok(candidate)
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

    fn checkpoint(
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        if control.is_cancelled() {
            Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::Cancelled,
            ))
        } else if control.is_deadline_exceeded() {
            Err(CodeIndexProductionErrorV1::Interrupted(
                CodeIndexInterruptionV1::DeadlineExceeded,
            ))
        } else {
            Ok(())
        }
    }

    fn interruption_error(control: &dyn CodeIndexExecutionControlV1) -> CodeIndexProductionErrorV1 {
        if control.is_deadline_exceeded() {
            CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
        } else {
            CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
        }
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
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
        file: &SanitizedCodeFileV1,
        captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        control: &dyn CodeIndexExecutionControlV1,
        worker: &crate::hotpath_observe::WorkerBusyGuard,
    ) -> Result<(ManifestDigest, Arc<FileGenerationArtifactsV1>), CodeIndexProductionErrorV1> {
        crate::hotpath_observe::measure_hot_loop!("code_index.materialize.file", {
            Self::checkpoint(control)?;
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
                Self::physical_reuse_key(config, file, descriptor, captured.sensitivity_level)?;
            if let Some(reused) =
                physical_artifacts.reuse(&physical_reuse_key, &receipt_bound, worker)
            {
                crate::hotpath_observe::add_reused_parses(1);
                Self::checkpoint(control)?;
                return Ok((physical_reuse_key, reused));
            }
            let snapshot = &capability.snapshot().snapshot;
            let parser = extractor
                .resolve_parser(receipt_bound.validated_file(), descriptor)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Extraction(
                        ExtractionFailureV1::GrammarUnavailable {
                            language: descriptor.language.clone(),
                        },
                    )
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
                config,
                snapshot,
                repository_parse_identity,
                file,
                captured,
                parser,
            ) {
                Ok((parse_artifacts, parsed_len)) => {
                    Self::checkpoint(control)?;
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
                                Self::interruption_error(control)
                            }
                            error => CodeIndexProductionErrorV1::Extraction(error),
                        })?
                }
                // One file exceeding the bounded parse budget is evidence about
                // that file, never about the generation: record it as a typed
                // unsupported document with a reason and keep building, instead
                // of failing the whole reconcile cycle and leaving the served
                // generation permanently stale.
                Err(CodeIndexProductionErrorV1::RetainedParse(ParseError::TimedOut { .. })) => {
                    Self::checkpoint(control)?;
                    extractor
                        .extract_parse_timed_out(&receipt_bound, descriptor)
                        .map_err(CodeIndexProductionErrorV1::Extraction)?
                }
                Err(error) => return Err(error),
            };
            Self::checkpoint(control)?;
            let (artifacts, exact_authority) = chunker
                .index_file_with_authority_from_extraction(
                    &receipt_bound,
                    &extraction,
                    descriptor,
                    captured.sensitivity_level,
                    &cancellation,
                )
                .map_err(|error| match error {
                    ChunkingFailureV1::Cancelled => Self::interruption_error(control),
                    error => CodeIndexProductionErrorV1::Chunk(error),
                })?;
            Self::checkpoint(control)?;
            let (authority, extraction, _) = extraction.into_parts();
            let artifact = Arc::new(FileGenerationArtifactsV1 {
                authority,
                extraction,
                artifacts,
                exact_authority,
            });
            Ok((physical_reuse_key, artifact))
        })
    }

    fn physical_reuse_key(
        config: &CodeIndexProductionConfigV1,
        file: &SanitizedCodeFileV1,
        descriptor: &tracedecay_domain::LanguageDescriptorV1,
        sensitivity_level: SensitivityLevelV1,
    ) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
        crate::hotpath_observe::measure_hot_loop!("code_index.materialize.reuse_key", {
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

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "code_index.build.materialize_full")]
    fn materialize_full(
        &self,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
        snapshot: &SanitizedCodeSnapshotV1,
        captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<StagedGenerationV1, CodeIndexProductionErrorV1> {
        let present_files = snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .collect::<Vec<_>>();
        let config = &self.config;
        let physical_artifacts = &self.physical_artifacts;
        let retained_parses = &self.retained_parses;
        let extracted = hotpath::measure_block!(
            "code_index.collect.materialize_full",
            collect_bounded_ordered(&present_files, |file, worker| {
                Self::extract_file(
                    config,
                    physical_artifacts,
                    retained_parses,
                    intake,
                    capability,
                    manifest,
                    extractor,
                    chunker,
                    repository_parse_identity,
                    file,
                    captured_files,
                    control,
                    worker,
                )
            })
        )?;
        // Parallel completion order is intentionally not cache authority.
        // Record artifacts in canonical snapshot order so bounded eviction and
        // subsequent physical reuse remain deterministic.
        let mut files = Vec::with_capacity(extracted.len());
        for (reuse_key, artifact) in extracted {
            Self::checkpoint(control)?;
            physical_artifacts.insert(reuse_key, &artifact);
            files.push(artifact);
        }
        Self::checkpoint(control)?;
        staged_generation(manifest.generation_id.clone(), files, Vec::new())
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "code_index.build.materialize_increment")]
    fn materialize_increment(
        &self,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
        repository_parse_identity: &CodeIndexRepositoryParseIdentityV1,
        active: &CodeIndexPublishedGenerationV1,
        increment: &super::generations::GenerationIncrementPlanV1,
        captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<StagedGenerationV1, CodeIndexProductionErrorV1> {
        let prior_by_occurrence = active
            .files
            .iter()
            .map(|file| {
                (
                    file.artifacts.chunks.document.file_occurrence_id.clone(),
                    file,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let current_by_occurrence = capability
            .snapshot()
            .snapshot
            .files
            .iter()
            .map(|file| (file.file_occurrence_id.clone(), file))
            .collect::<BTreeMap<_, _>>();
        let scope = CodeIndexGenerationScopeV1::for_snapshot(&capability.snapshot().snapshot);
        let config = &self.config;
        let physical_artifacts = &self.physical_artifacts;
        let retained_parses = &self.retained_parses;
        let file_materializations = hotpath::measure_block!(
            "code_index.collect.materialize_increment",
            collect_bounded_ordered(
                &increment.files,
                |file_plan,
                 worker|
                 -> Result<IncrementFileMaterializationV1, CodeIndexProductionErrorV1> {
                Self::checkpoint(control)?;
                match &file_plan.action {
                    FileExtractionActionV1::CarryForward {
                        file_occurrence_id,
                        prior_file_occurrence_id,
                        ..
                    } => {
                        let prior = prior_by_occurrence
                            .get(prior_file_occurrence_id)
                            .ok_or_else(|| {
                                CodeIndexProductionErrorV1::Contract(
                                    "increment plan refers to a missing prior file".to_owned(),
                                )
                            })?;
                        let current_file = current_by_occurrence
                            .get(file_occurrence_id)
                            .ok_or_else(|| {
                                CodeIndexProductionErrorV1::Contract(
                                    "increment plan refers to a missing current file".to_owned(),
                                )
                            })?;
                        let carried = if let Some(captured) = captured_files.get(file_occurrence_id)
                        {
                            let receipt_bound = intake
                                .bind_file(
                                    capability,
                                    &config.project_id,
                                    ValidatedCodeFileV1 {
                                        generation_id: manifest.generation_id.clone(),
                                        file: (**current_file).clone(),
                                        snapshot_digest: capability
                                            .snapshot()
                                            .intake_digest
                                            .clone(),
                                        sanitized_bytes: captured.sanitized_bytes.to_vec(),
                                    },
                                )
                                .map_err(CodeIndexProductionErrorV1::Intake)?;
                            prior.rematerialize_for_file(&receipt_bound)
                        } else {
                            prior.rematerialize_carried_forward(
                                config,
                                &scope,
                                &manifest.generation_id,
                                current_file,
                            )
                        };
                        if let Ok(artifact) = carried {
                            crate::hotpath_observe::add_reused_parses(1);
                            Ok(IncrementFileMaterializationV1::CarryForward(Arc::new(
                                artifact,
                            )))
                        } else {
                            // Opaque exact evidence may refuse generation-local
                            // occurrence rebinding. Re-extract through the parser
                            // authority instead of rewriting that evidence.
                            let file = current_file;
                            let (reuse_key, artifact) = Self::extract_file(
                                config,
                                physical_artifacts,
                                retained_parses,
                                intake,
                                capability,
                                manifest,
                                extractor,
                                chunker,
                                repository_parse_identity,
                                file,
                                captured_files,
                                control,
                                worker,
                            )?;
                            Ok(IncrementFileMaterializationV1::ReExtracted {
                                reuse_key,
                                artifact,
                                fallback: true,
                            })
                        }
                    }
                    FileExtractionActionV1::ReExtract { file } => {
                        let (reuse_key, artifact) = Self::extract_file(
                            config,
                            physical_artifacts,
                            retained_parses,
                            intake,
                            capability,
                            manifest,
                            extractor,
                            chunker,
                            repository_parse_identity,
                            file,
                            captured_files,
                            control,
                            worker,
                        )?;
                        Ok(IncrementFileMaterializationV1::ReExtracted {
                            reuse_key,
                            artifact,
                            fallback: false,
                        })
                    }
                    FileExtractionActionV1::Deleted { .. } => {
                        Ok(IncrementFileMaterializationV1::Deleted)
                    }
                }
                },
            )
        )?;

        let mut files = Vec::new();
        let mut reextracted_files = Vec::new();
        let mut reextracted_symbols = Vec::new();
        let mut used_reextraction_fallback = false;

        for materialization in file_materializations {
            Self::checkpoint(control)?;
            match materialization {
                IncrementFileMaterializationV1::CarryForward(artifact) => files.push(artifact),
                IncrementFileMaterializationV1::ReExtracted {
                    reuse_key,
                    artifact,
                    fallback,
                } => {
                    physical_artifacts.insert(reuse_key, &artifact);
                    used_reextraction_fallback |= fallback;
                    if !fallback {
                        reextracted_files.push(artifact.artifacts.chunks.clone());
                        reextracted_symbols.extend(artifact.artifacts.symbols.clone());
                    }
                    files.push(artifact);
                }
                IncrementFileMaterializationV1::Deleted => {}
            }
        }
        Self::checkpoint(control)?;

        if used_reextraction_fallback {
            let mut staged = staged_generation(manifest.generation_id.clone(), files, Vec::new())?;
            staged.lineage = SymbolLineageResolver::new()
                .resolve(&active.symbols, &staged.symbols)
                .map_err(CodeIndexProductionErrorV1::Lineage)?;
            return Ok(staged);
        }

        let prior_files = active
            .files
            .iter()
            .map(|file| file.artifacts.chunks.clone())
            .collect::<Vec<_>>();
        let materialized = materialize_generation_increment(
            increment,
            manifest.generation_id.clone(),
            &prior_files,
            reextracted_files,
            &active.symbols,
            reextracted_symbols,
        )
        .map_err(CodeIndexProductionErrorV1::Increment)?;
        let expected = staged_generation(
            manifest.generation_id.clone(),
            files,
            materialized.lineage.clone(),
        )?;
        if expected.chunks != materialized.chunks || expected.symbols != materialized.symbols {
            return Err(CodeIndexProductionErrorV1::Contract(
                "incremental materialization disagrees with file evidence".to_owned(),
            ));
        }
        Ok(StagedGenerationV1 {
            chunks: materialized.chunks,
            symbols: materialized.symbols,
            lineage: materialized.lineage,
            files: expected.files,
        })
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
