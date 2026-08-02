//! Production composition for the Plan 25 code-index contracts.
//!
//! This module owns the one in-process vertical from receipt-bound capture to
//! a store-owned atomic publication handoff. It does not open files, databases,
//! or network clients: capture provides sanitized bytes, the projection and
//! publication ports own their respective effects, and restart restores only
//! the immutable generation returned by the publication port.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeGenerationManifestV1,
    CodeIndexCapabilityManifestV1, CodeSearchEligibilityV1, ComponentVersion, CoverageSummaryV1,
    ExtractionBatchV1, ExtractionFailureV1, FileOccurrenceId, GenerationTestAttributionV1,
    IntakeRejectionV1, ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionReplayReasonV1,
    ProviderEvaluationStateV1, RefId, RepositoryId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, SymbolLineageCandidateV1,
    SymbolOccurrenceId, TestAttributionEvidenceClassV1, UtcMicros, ValidatedCodeFileV1,
    ValidatedCodeSnapshotV1, WorktreeId, canonical_sha256,
};

use super::{
    capabilities::{BaseCapabilityEmitter, CapabilityEmissionErrorV1, CodeIndexCapabilityEmitter},
    chunks::{
        ChunkingFailureV1, CodeFileIndexArtifactsV1, CodeIndexEdgeAbstentionV1,
        DeterministicCodeChunker, ExactExtractionAuthorityV1, ExtractionAdmittedCodeSearchChunkV1,
        content_digest,
    },
    extract::{
        ExtractionCancellation, LanguageExtractor, TreeSitterExtractor, rebind_extraction_batch,
    },
    generations::{
        FileExtractionActionV1, GenerationPlanner, GenerationPlanningErrorV1, RebuildTriggerV1,
    },
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
    test_attribution::{
        GenerationTestJoinV1, TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1,
        TestAttributionWatermarkV1,
    },
};

mod helpers;
use helpers::*;

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

/// One sanitized byte payload paired with immutable snapshot metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeIndexCapturedFileV1 {
    pub file_occurrence_id: FileOccurrenceId,
    pub sanitized_bytes: Vec<u8>,
    pub sensitivity_level: SensitivityLevelV1,
}

/// Inputs for one complete immutable code-index generation.
#[derive(Clone, Debug)]
pub struct CodeIndexBuildRequestV1 {
    pub snapshot: SanitizedCodeSnapshotV1,
    pub captured_files: Vec<CodeIndexCapturedFileV1>,
    /// Capture-reported paths are evidence only; digest equality remains the
    /// sole reuse authority.
    pub changed_files: BTreeSet<String>,
    /// Additional conservative invalidations that the application boundary,
    /// rather than the sanitized snapshot, is authoritative to report.
    pub invalidations: BTreeSet<RebuildTriggerV1>,
    pub sealed_at: UtcMicros,
    pub target_projection_key: ProjectionKeyV1,
}

/// Synchronous checkpoints exposed by an application/daemon request.
pub trait CodeIndexExecutionControlV1: Sync {
    fn is_cancelled(&self) -> bool;
    fn is_deadline_exceeded(&self) -> bool;
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

    fn validate(&self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.repository
            .validate()
            .and_then(|()| self.reference.as_ref().map_or(Ok(()), RefId::validate))
            .and_then(|()| self.worktree.as_ref().map_or(Ok(()), WorktreeId::validate))
            .map_err(|error| CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string()))
    }
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
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1>;
}

#[derive(Clone, Debug)]
struct FileGenerationArtifactsV1 {
    authority: ReceiptBoundCodeFileAuthorityV1,
    extraction: ExtractionBatchV1,
    artifacts: CodeFileIndexArtifactsV1,
    exact_authority: ExactExtractionAuthorityV1,
}

enum IncrementFileMaterializationV1 {
    CarryForward(FileGenerationArtifactsV1),
    ReExtracted {
        file: SanitizedCodeFileV1,
        artifact: FileGenerationArtifactsV1,
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
fn collect_bounded_ordered<T, R, E, F>(items: &[T], operation: F) -> Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: Send,
    F: Fn(&T) -> Result<R, E> + Sync,
{
    // Always enter the indexing pool, even when the width is 1: the pool is
    // what keeps the nested chunk-level fan-out inside the reservation
    // instead of spilling onto rayon's global (all-cores) pool.
    crate::parallelism::install(|| {
        if items.len() < 2 || crate::parallelism::indexing_workers() < 2 {
            return items.iter().map(&operation).collect();
        }
        let results: Vec<Result<R, E>> = items
            .par_iter()
            .map(&operation)
            .collect::<Vec<Result<R, E>>>();
        results.into_iter().collect()
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhysicalCodeArtifactPoolStatsV1 {
    pub inserted: u64,
    pub reused: u64,
}

#[derive(Default)]
struct PhysicalCodeArtifactPoolStateV1 {
    artifacts: BTreeMap<ManifestDigest, Arc<FileGenerationArtifactsV1>>,
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

impl SharedPhysicalCodeArtifactPoolV1 {
    fn reuse(
        &self,
        key: &ManifestDigest,
        file: &ReceiptBoundCodeFileV1,
    ) -> Option<FileGenerationArtifactsV1> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let artifact = state.artifacts.get(key)?.clone();
        let rebound = artifact.rematerialize_for_file(file).ok()?;
        state.reused = state.reused.saturating_add(1);
        Some(rebound)
    }

    fn insert(&self, key: ManifestDigest, artifact: FileGenerationArtifactsV1) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.artifacts.contains_key(&key) {
            return;
        }
        while state.artifacts.len() >= MAX_PHYSICAL_CODE_ARTIFACTS {
            let Some(evicted) = state.insertion_order.pop_front() else {
                break;
            };
            state.artifacts.remove(&evicted);
        }
        state.insertion_order.push_back(key.clone());
        state.artifacts.insert(key, Arc::new(artifact));
        state.inserted = state.inserted.saturating_add(1);
    }

    pub fn stats(&self) -> PhysicalCodeArtifactPoolStatsV1 {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PhysicalCodeArtifactPoolStatsV1 {
            inserted: state.inserted,
            reused: state.reused,
        }
    }
}

const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileGenerationArtifactsV1 {
    authority: ReceiptBoundCodeFileAuthorityV1,
    extraction: ExtractionBatchV1,
    artifacts: CodeFileIndexArtifactsV1,
}

#[derive(Serialize)]
struct PersistedFileGenerationArtifactsRefV1<'a> {
    authority: &'a ReceiptBoundCodeFileAuthorityV1,
    extraction: &'a ExtractionBatchV1,
    artifacts: &'a CodeFileIndexArtifactsV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPublishedGenerationV1 {
    format_revision: u32,
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    files: Vec<PersistedFileGenerationArtifactsV1>,
    lineage: Vec<SymbolLineageCandidateV1>,
    coverage: CoverageSummaryV1,
    capability: CodeIndexCapabilityManifestV1,
    projection_request: ProjectionBatchRequestV1,
    projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Serialize)]
struct PersistedPublishedGenerationRefV1<'a> {
    format_revision: u32,
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    files: Vec<PersistedFileGenerationArtifactsRefV1<'a>>,
    lineage: &'a [SymbolLineageCandidateV1],
    coverage: CoverageSummaryV1,
    capability: &'a CodeIndexCapabilityManifestV1,
    projection_request: &'a ProjectionBatchRequestV1,
    projection_receipt: &'a ProjectionBatchReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedPublishedGenerationEnvelopeV1 {
    state_digest: ManifestDigest,
    generation: PersistedPublishedGenerationV1,
}

#[derive(Deserialize)]
struct SealedPublishedGenerationFormatProbeV1 {
    generation: PersistedPublishedGenerationFormatProbeV1,
}

#[derive(Deserialize)]
struct PersistedPublishedGenerationFormatProbeV1 {
    format_revision: u32,
}

#[derive(Serialize)]
struct SealedPublishedGenerationEnvelopeRefV1<'a> {
    state_digest: &'a ManifestDigest,
    generation: PersistedPublishedGenerationRefV1<'a>,
}

impl FileGenerationArtifactsV1 {
    fn rematerialize_for_file(
        &self,
        file: &ReceiptBoundCodeFileV1,
    ) -> Result<Self, ChunkingFailureV1> {
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
    files: Vec<FileGenerationArtifactsV1>,
    chunks: GenerationChunkManifestV1,
    symbols: GenerationSymbolIndexV1,
    lineage: Vec<SymbolLineageCandidateV1>,
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
    /// Amortized parser-backed exact admission. `admit_all` re-canonicalizes and
    /// re-hashes every chunk, which is pure waste on the serving path once the
    /// immutable chunk set has been admitted. Only success is cached.
    admitted: OnceLock<Vec<ExtractionAdmittedCodeSearchChunkV1>>,
    /// Amortized test-attribution join. Query admission rebuilds this authority
    /// per call even when the generation is unchanged; the traversal and its
    /// evidence digest are a pure function of the immutable generation. Only
    /// success is cached.
    attribution: OnceLock<PublishedGenerationTestAttributionAuthorityV1>,
}

/// Immutable test-attribution reader derived from one sealed production code
/// generation. The reader owns no second graph or test store: it projects
/// conservative candidates from the generation's canonical relation graph and
/// retains the exact generation/test watermark produced at construction.
#[derive(Clone, Debug)]
pub struct PublishedGenerationTestAttributionAuthorityV1 {
    generation_id: CodeGenerationId,
    read: GenerationProviderReadV1<GenerationTestJoinV1>,
}

impl GenerationTestAttributionJoinReadPort for PublishedGenerationTestAttributionAuthorityV1 {
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        if generation == &self.generation_id {
            self.read.clone()
        } else {
            GenerationProviderReadV1::new(
                ProviderEvaluationStateV1::Stale,
                GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static stale attribution read"))
        }
    }
}

impl CodeIndexPublishedGenerationV1 {
    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.manifest
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.snapshot
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

    /// Whether this generation's parser-backed exact admission has already been
    /// computed.
    ///
    /// [`Self::admitted_chunks`] re-canonicalizes and re-hashes every chunk on
    /// its first call, so whoever calls it first pays an O(store) sweep. This
    /// lets an activation path prove it warmed that memo instead of leaving the
    /// cost to the first query. Like [`Self::is_validated`] it reports memo
    /// state only and never short-circuits a gate.
    pub fn is_exact_admission_warm(&self) -> bool {
        self.admitted.get().is_some()
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

    /// Return chunks re-admitted through their parser-backed exact authority.
    /// Downstream exact/phrase/BM25 projections must consume this value rather
    /// than raw chunks, preserving the non-demotable exact tier.
    pub fn admitted_chunks(
        &self,
    ) -> Result<Vec<ExtractionAdmittedCodeSearchChunkV1>, ChunkingFailureV1> {
        if let Some(admitted) = self.admitted.get() {
            return Ok(admitted.clone());
        }
        let mut chunks = Vec::new();
        for file in &self.files {
            chunks.extend(
                file.exact_authority
                    .admit_all(file.artifacts.chunks.chunks.clone())?,
            );
        }
        chunks.sort_by(|left, right| left.chunk().id.cmp(&right.chunk().id));
        let _ = self.admitted.set(chunks.clone());
        Ok(chunks)
    }

    /// Encode the complete sealed generation for immutable store publication.
    ///
    /// Exact-admission authority internals are deliberately omitted. They are
    /// recomputed from the validated parser-produced chunks during restore.
    pub fn encode_sealed(&self) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        self.validate()?;
        let generation = PersistedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            files: self
                .files
                .iter()
                .map(|file| PersistedFileGenerationArtifactsRefV1 {
                    authority: &file.authority,
                    extraction: &file.extraction,
                    artifacts: &file.artifacts,
                })
                .collect(),
            lineage: &self.lineage,
            coverage: self.coverage,
            capability: &self.capability,
            projection_request: self.projection.request(),
            projection_receipt: self.projection.receipt(),
        };
        let state_digest = canonical_sha256(&generation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        serde_json::to_vec(&SealedPublishedGenerationEnvelopeRefV1 {
            state_digest: &state_digest,
            generation,
        })
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation serialization failed: {error}"
            ))
        })
    }

    /// Restore a complete sealed generation and repeat every canonical
    /// generation, chunk, graph, capability, and projection receipt check.
    pub fn decode_sealed(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        if !Self::sealed_format_is_compatible(bytes)? {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation format revision is incompatible".to_owned(),
            ));
        }
        let envelope: SealedPublishedGenerationEnvelopeV1 =
            serde_json::from_slice(bytes).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation decoding failed: {error}"
                ))
            })?;
        let expected_digest = canonical_sha256(&envelope.generation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if expected_digest != envelope.state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match its payload".to_owned(),
            ));
        }

        let mut files = Vec::with_capacity(envelope.generation.files.len());
        for file in envelope.generation.files {
            let exact_authority = ExactExtractionAuthorityV1::restore(&file.artifacts.chunks)
                .map_err(CodeIndexProductionErrorV1::Chunk)?;
            files.push(FileGenerationArtifactsV1 {
                authority: file.authority,
                extraction: file.extraction,
                artifacts: file.artifacts,
                exact_authority,
            });
        }
        let chunks = GenerationChunkManifestV1::new(
            envelope.generation.manifest.generation_id.clone(),
            files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect(),
        )
        .map_err(CodeIndexProductionErrorV1::Increment)?;
        let symbols = GenerationSymbolIndexV1::new(
            envelope.generation.manifest.generation_id.clone(),
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.clone())
                .collect(),
        )
        .map_err(CodeIndexProductionErrorV1::Lineage)?;
        let (edges, edge_abstentions) = collect_edge_evidence(&files);
        let projection = ProjectionPublicationHandoffV1::restore(
            envelope.generation.projection_request,
            envelope.generation.projection_receipt,
        )
        .map_err(CodeIndexProductionErrorV1::Projection)?;
        let generation = Self {
            manifest: envelope.generation.manifest,
            snapshot: envelope.generation.snapshot,
            files,
            chunks,
            symbols,
            lineage: envelope.generation.lineage,
            edges,
            edge_abstentions,
            coverage: envelope.generation.coverage,
            capability: envelope.generation.capability,
            projection,
            validated: OnceLock::new(),
            admitted: OnceLock::new(),
            attribution: OnceLock::new(),
        };
        // Bytes were just re-read from the sealed store, so this restore must
        // repeat every canonical check rather than trust a memoized verdict.
        generation.validate_fresh()?;
        Ok(generation)
    }

    pub fn sealed_format_is_compatible(bytes: &[u8]) -> Result<bool, CodeIndexProductionErrorV1> {
        let probe: SealedPublishedGenerationFormatProbeV1 =
            serde_json::from_slice(bytes).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation format probe failed: {error}"
                ))
            })?;
        Ok(probe.generation.format_revision == SEALED_GENERATION_FORMAT_REVISION_V1)
    }

    /// Amortized integrity gate for an already-constructed generation.
    ///
    /// The first call runs every canonical check; later calls are O(1). This is
    /// sound because a published generation is immutable: no field can change
    /// after construction, so re-validating identical bytes cannot change the
    /// answer. It is fail-closed because only success is memoized — a
    /// generation that has never validated still runs the full check, and a
    /// generation that fails keeps failing on every subsequent call.
    fn validate(&self) -> Result<(), CodeIndexProductionErrorV1> {
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

        let mut files = self.files.iter().collect::<Vec<_>>();
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
        for file in &files {
            file.artifacts
                .validate()
                .map_err(CodeIndexProductionErrorV1::Chunk)?;
            let occurrence = self.snapshot.files.iter().find(|candidate| {
                candidate.file_occurrence_id == file.artifacts.chunks.document.file_occurrence_id
            });
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
        }
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
        let mut edges = files
            .iter()
            .flat_map(|file| file.artifacts.edges.iter())
            .collect::<Vec<_>>();
        edges.sort_by(|left, right| edge_order(left, right));
        let edges_match = edges.len() == self.edges.len()
            && edges
                .iter()
                .zip(&self.edges)
                .all(|(left, right)| *left == right);
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
        Ok(())
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
    #[error("code-index contract failed: {0}")]
    Contract(String),
}

/// Production owner for one repository and one atomic publication authority.
pub struct CodeIndexProductionOwnerV1<P, S> {
    config: CodeIndexProductionConfigV1,
    publication: P,
    projection: S,
    physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
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
        })
    }

    pub fn with_physical_artifact_pool(
        mut self,
        physical_artifacts: SharedPhysicalCodeArtifactPoolV1,
    ) -> Self {
        self.physical_artifacts = physical_artifacts;
        self
    }

    /// Load the currently active immutable generation. A restart therefore
    /// resumes from the publication authority rather than mutable worker state.
    pub fn active_generation(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexProductionErrorV1> {
        scope.validate()?;
        if scope.repository != self.config.repository {
            return Err(CodeIndexProductionErrorV1::Contract(
                "generation scope is foreign to the production owner's repository store".to_owned(),
            ));
        }
        let active = self.publication.load_active(scope)?;
        if let Some(active) = &active {
            active.validate()?;
            if active.manifest.project_id != self.config.project_id
                || CodeIndexGenerationScopeV1::for_snapshot(&active.snapshot) != *scope
                || active.manifest.sanitizer_revision != self.config.sanitizer_revision
                || active.manifest.chunker_revision != self.config.chunker_revision
                || active.manifest.privacy_domain != self.config.privacy_domain
                || active.manifest.privacy_key_epoch != self.config.privacy_key_epoch
                || active
                    .chunks
                    .chunks()
                    .iter()
                    .any(|chunk| chunk.sensitivity.policy_revision != self.config.policy_revision)
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "active generation is incompatible with the production owner configuration"
                        .to_owned(),
                ));
            }
        }
        Ok(active)
    }

    /// Build one complete generation and atomically publish it only after
    /// intake, parser evidence, lineage, exact admission, projection receipt,
    /// and capability validation have all succeeded.
    pub fn build_and_publish(
        &mut self,
        request: CodeIndexBuildRequestV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
        Self::checkpoint(control)?;
        let scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
        let active = self.active_generation(&scope)?;
        Self::checkpoint(control)?;

        let intake = self.intake_at(request.sealed_at, registry_for_snapshot(&request.snapshot)?);
        let capability = intake
            .admit(request.snapshot.clone())
            .map_err(CodeIndexProductionErrorV1::Intake)?;
        let validated = capability.snapshot().clone();
        let captured_files = captured_files(&validated.snapshot, request.captured_files)?;
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
        let staged = match (active.as_ref(), increment.as_ref()) {
            (Some(active), Some(increment)) => self.materialize_increment(
                &intake,
                &capability,
                &manifest,
                &extractor,
                &chunker,
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

        let (edges, edge_abstentions) = collect_edge_evidence(&staged.files);
        let candidate = CodeIndexPublishedGenerationV1 {
            manifest,
            snapshot: validated.snapshot,
            files: staged.files,
            chunks: staged.chunks,
            symbols: staged.symbols,
            lineage: staged.lineage,
            edges,
            edge_abstentions,
            coverage,
            capability,
            projection,
            validated: OnceLock::new(),
            admitted: OnceLock::new(),
            attribution: OnceLock::new(),
        };
        candidate.validate()?;

        let expected = active
            .as_ref()
            .map(|generation| generation.manifest.generation_id.clone());
        self.publication
            .publish_atomically(&scope, expected.as_ref(), candidate.clone())?;
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

    #[allow(clippy::too_many_arguments)]
    fn extract_file(
        config: &CodeIndexProductionConfigV1,
        physical_artifacts: &SharedPhysicalCodeArtifactPoolV1,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
        file: &SanitizedCodeFileV1,
        captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        control: &dyn CodeIndexExecutionControlV1,
        record_physical_artifact: bool,
    ) -> Result<FileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
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
                    sanitized_bytes: captured.sanitized_bytes.clone(),
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
        if let Some(reused) = physical_artifacts.reuse(&physical_reuse_key, &receipt_bound) {
            Self::checkpoint(control)?;
            return Ok(reused);
        }
        let cancellation = ExtractionControlBridge { control };
        let extraction = extractor
            .extract(&receipt_bound, descriptor, &cancellation)
            .map_err(|error| match error {
                ExtractionFailureV1::Cancelled | ExtractionFailureV1::TimedOut => {
                    Self::interruption_error(control)
                }
                error => CodeIndexProductionErrorV1::Extraction(error),
            })?;
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
        let artifact = FileGenerationArtifactsV1 {
            authority,
            extraction,
            artifacts,
            exact_authority,
        };
        if record_physical_artifact {
            physical_artifacts.insert(physical_reuse_key, artifact.clone());
        }
        Ok(artifact)
    }

    fn physical_reuse_key(
        config: &CodeIndexProductionConfigV1,
        file: &SanitizedCodeFileV1,
        descriptor: &tracedecay_domain::LanguageDescriptorV1,
        sensitivity_level: SensitivityLevelV1,
    ) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
        canonical_sha256(&(
            PHYSICAL_CODE_ARTIFACT_REUSE_DIGEST_DOMAIN,
            &config.project_id,
            &config.repository,
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
    }

    fn record_physical_artifact(
        config: &CodeIndexProductionConfigV1,
        physical_artifacts: &SharedPhysicalCodeArtifactPoolV1,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        file: &SanitizedCodeFileV1,
        captured_files: &BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
        artifact: &FileGenerationArtifactsV1,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let captured = captured_files
            .get(&file.file_occurrence_id)
            .ok_or(CodeIndexInputErrorV1::MissingCapturedFile)?;
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
        physical_artifacts.insert(physical_reuse_key, artifact.clone());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_full(
        &self,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
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
        let files = collect_bounded_ordered(&present_files, |file| {
            Self::extract_file(
                config,
                physical_artifacts,
                intake,
                capability,
                manifest,
                extractor,
                chunker,
                file,
                captured_files,
                control,
                false,
            )
        })?;
        // Parallel completion order is intentionally not cache authority.
        // Record artifacts in canonical snapshot order so bounded eviction and
        // subsequent physical reuse remain deterministic.
        for (file, artifact) in present_files.into_iter().zip(&files) {
            Self::checkpoint(control)?;
            Self::record_physical_artifact(
                config,
                physical_artifacts,
                intake,
                file,
                captured_files,
                artifact,
            )?;
        }
        Self::checkpoint(control)?;
        staged_generation(manifest.generation_id.clone(), files, Vec::new())
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_increment(
        &self,
        intake: &SanitizedCodeIntake<StaticLanguageRegistry>,
        capability: &SanitizedSnapshotCapabilityV1,
        manifest: &CodeGenerationManifestV1,
        extractor: &TreeSitterExtractor,
        chunker: &DeterministicCodeChunker,
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
        let config = &self.config;
        let physical_artifacts = &self.physical_artifacts;
        let file_materializations = collect_bounded_ordered(
            &increment.files,
            |file_plan| -> Result<IncrementFileMaterializationV1, CodeIndexProductionErrorV1> {
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
                        let captured = captured_files
                            .get(file_occurrence_id)
                            .ok_or(CodeIndexInputErrorV1::MissingCapturedFile)?;
                        let receipt_bound = intake
                            .bind_file(
                                capability,
                                &config.project_id,
                                ValidatedCodeFileV1 {
                                    generation_id: manifest.generation_id.clone(),
                                    file: (**current_file).clone(),
                                    snapshot_digest: capability.snapshot().intake_digest.clone(),
                                    sanitized_bytes: captured.sanitized_bytes.clone(),
                                },
                            )
                            .map_err(CodeIndexProductionErrorV1::Intake)?;
                        if let Ok(artifact) = prior.rematerialize_for_file(&receipt_bound) {
                            Ok(IncrementFileMaterializationV1::CarryForward(artifact))
                        } else {
                            // Opaque exact evidence may refuse generation-local
                            // occurrence rebinding. Re-extract through the parser
                            // authority instead of rewriting that evidence.
                            let file = current_file;
                            let artifact = Self::extract_file(
                                config,
                                physical_artifacts,
                                intake,
                                capability,
                                manifest,
                                extractor,
                                chunker,
                                file,
                                captured_files,
                                control,
                                false,
                            )?;
                            Ok(IncrementFileMaterializationV1::ReExtracted {
                                file: (**file).clone(),
                                artifact,
                                fallback: true,
                            })
                        }
                    }
                    FileExtractionActionV1::ReExtract { file } => {
                        let artifact = Self::extract_file(
                            config,
                            physical_artifacts,
                            intake,
                            capability,
                            manifest,
                            extractor,
                            chunker,
                            file,
                            captured_files,
                            control,
                            false,
                        )?;
                        Ok(IncrementFileMaterializationV1::ReExtracted {
                            file: file.clone(),
                            artifact,
                            fallback: false,
                        })
                    }
                    FileExtractionActionV1::Deleted { .. } => {
                        Ok(IncrementFileMaterializationV1::Deleted)
                    }
                }
            },
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
                    file,
                    artifact,
                    fallback,
                } => {
                    Self::record_physical_artifact(
                        config,
                        physical_artifacts,
                        intake,
                        &file,
                        captured_files,
                        &artifact,
                    )?;
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
