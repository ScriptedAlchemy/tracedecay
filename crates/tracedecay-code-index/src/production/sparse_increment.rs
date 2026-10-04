//! A successor generation sealed over its parent from the files an edit
//! changed.
//!
//! An in-place edit replaces a few files of a sealed generation. Everything
//! the successor seals is either a segment the parent already stores, kept
//! by its content address, or a function of the replaced files and the
//! sites resolution re-decides for them: the edited files' segments, the
//! cross-file evidence and graph pages of the files whose resolution moved,
//! the resolution index pages the edited names reach, and the generation
//! evidence. Aggregate facts (statistics, coverage, chunk count and policy
//! census) move by the replaced files' terms. So the successor decodes the
//! edited files on both sides, the files their names reach, and the parent
//! pages it reads; it never decodes the parent.
//!
//! An edit that adds, removes, or renames a file, or moves what name lookups
//! land in, needs every file's resolution and is built cold instead.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, CodeGenerationId, CodeGenerationManifestV1,
    CodeGenerationSourceCommitmentsV1, FileOccurrenceId, ManifestDigest, ProjectionKeyV1,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1,
};

use super::ignored_sources::IgnoredSourceRosterV1;
use super::partitioned_codec::{
    PartitionedCodeGraphPageDescriptorV1, PartitionedFileSegmentDescriptorV1,
    PartitionedGenerationEvidenceRefV1, PartitionedPublishedGenerationRefV1,
    SealedGenerationSegmentPublicationV1, seal_partitioned_manifest, write_generation_evidence,
};
use super::projection_rows::FileChunkRostersV1;
use super::resolution_index::reseal_resolution_index;
use super::sealed_parent::SealedParentGenerationV1;
use super::sparse_resolution::moves_name_lookups;
use super::sparse_successor::{CrossFileEdgeCountsV1, SealedSuccessorV1};
use super::*;
use crate::generations::GenerationIncrementPlanV1;

/// Why a generation was built whole rather than over its sealed parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexColdBuildReasonV1 {
    /// No compatible sealed parent.
    NoParent,
    /// A declared or inferred rebuild trigger invalidates every file.
    FullRebuild,
    /// The projection profile changed, so every chunk replays.
    ProjectionKeyChange,
    /// Files were added, removed, or renamed, which moves module trees.
    FilesAddedOrRemoved,
    /// More than one file in eight changed.
    ChangedShare,
    /// An edited file changed its imports, package clauses, or a manifest,
    /// which decides where every name lookup lands.
    MovesNameLookups,
    /// An edited Go file declares method sets, and interface satisfaction
    /// pairs them with every other Go file's, including files that never
    /// name the edited file's types.
    MovesGoMethodSets,
}

impl CodeIndexColdBuildReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoParent => "no_parent",
            Self::FullRebuild => "full_rebuild",
            Self::ProjectionKeyChange => "projection_key_change",
            Self::FilesAddedOrRemoved => "files_added_or_removed",
            Self::ChangedShare => "changed_share",
            Self::MovesNameLookups => "moves_name_lookups",
            Self::MovesGoMethodSets => "moves_go_method_sets",
        }
    }
}

/// A changed file's share of a generation above which it seals cold: one in
/// this many, the same share a layered graph refresh carries.
const SPARSE_MAX_CHANGED_FILE_SHARE_DENOMINATOR: usize = 8;

/// One segment a sparse successor seals, in publication order.
enum SparseSegmentV1 {
    File(ManifestDigest, Vec<u8>),
    FileEvidence(ManifestDigest, Vec<u8>),
    ResolutionIndex(ManifestDigest, Vec<u8>),
    CodeGraphPage(u32, ManifestDigest, Vec<u8>),
    EvidencePage(u32, ManifestDigest, Vec<u8>),
    EvidenceCommit(ManifestDigest, u64, u32),
}

impl SparseSegmentV1 {
    fn of(publication: SealedGenerationSegmentPublicationV1<'_>) -> Self {
        match publication {
            SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                Self::File(digest.clone(), bytes.to_vec())
            }
            SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes } => {
                Self::FileEvidence(digest.clone(), bytes.to_vec())
            }
            SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => {
                Self::ResolutionIndex(digest.clone(), bytes.to_vec())
            }
            SealedGenerationSegmentPublicationV1::CodeGraphPage {
                file_key,
                page_digest,
                bytes,
            } => Self::CodeGraphPage(file_key, page_digest.clone(), bytes.to_vec()),
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal,
                page_digest,
                bytes,
            } => Self::EvidencePage(page_ordinal, page_digest.clone(), bytes.to_vec()),
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest,
                segment_size_bytes,
                page_count,
            } => Self::EvidenceCommit(segment_digest.clone(), segment_size_bytes, page_count),
        }
    }

    fn publication(&self) -> SealedGenerationSegmentPublicationV1<'_> {
        match self {
            Self::File(digest, bytes) => {
                SealedGenerationSegmentPublicationV1::File { digest, bytes }
            }
            Self::FileEvidence(digest, bytes) => {
                SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
            }
            Self::ResolutionIndex(digest, bytes) => {
                SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes }
            }
            Self::CodeGraphPage(file_key, page_digest, bytes) => {
                SealedGenerationSegmentPublicationV1::CodeGraphPage {
                    file_key: *file_key,
                    page_digest,
                    bytes,
                }
            }
            Self::EvidencePage(page_ordinal, page_digest, bytes) => {
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                    page_ordinal: *page_ordinal,
                    page_digest,
                    bytes,
                }
            }
            Self::EvidenceCommit(segment_digest, segment_size_bytes, page_count) => {
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                    segment_digest,
                    segment_size_bytes: *segment_size_bytes,
                    page_count: *page_count,
                }
            }
        }
    }
}

/// A successor generation sealed over its parent: its metadata, its
/// projection handoff, and the new segments and manifest a publication
/// store writes.
pub struct CodeIndexSparseGenerationV1 {
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    statistics: CodeIndexGenerationStatisticsV1,
    chunk_count: u64,
    projection: ProjectionPublicationHandoffV1,
    segments: Vec<SparseSegmentV1>,
    manifest_bytes: Vec<u8>,
    clone_payloads_reused: u64,
    clone_payloads_computed: u64,
    clone_stale_invalidations: u64,
}

impl std::fmt::Debug for CodeIndexSparseGenerationV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeIndexSparseGenerationV1")
            .field("generation_id", &self.manifest.generation_id)
            .field("segments", &self.segments.len())
            .finish_non_exhaustive()
    }
}

impl CodeIndexSparseGenerationV1 {
    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.manifest
    }

    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.snapshot
    }

    pub fn statistics(&self) -> &CodeIndexGenerationStatisticsV1 {
        &self.statistics
    }

    pub fn chunk_count(&self) -> u64 {
        self.chunk_count
    }

    pub fn projection(&self) -> &ProjectionPublicationHandoffV1 {
        &self.projection
    }

    pub fn clone_update_statistics(&self) -> (u64, u64, bool) {
        (
            self.clone_payloads_reused,
            self.clone_stale_invalidations,
            self.clone_payloads_computed > 0 || self.clone_stale_invalidations > 0,
        )
    }

    /// Every segment this successor wrote: its kind, content address, and
    /// stored size.
    #[cfg(test)]
    pub(super) fn written_segments(
        &self,
    ) -> impl Iterator<Item = (&'static str, &ManifestDigest, usize)> {
        self.segments.iter().filter_map(|segment| match segment {
            SparseSegmentV1::File(digest, bytes) => Some(("file", digest, bytes.len())),
            SparseSegmentV1::FileEvidence(digest, bytes) => {
                Some(("file_evidence", digest, bytes.len()))
            }
            SparseSegmentV1::ResolutionIndex(digest, bytes) => {
                Some(("resolution_index", digest, bytes.len()))
            }
            SparseSegmentV1::CodeGraphPage(_, digest, bytes) => {
                Some(("code_graph_page", digest, bytes.len()))
            }
            SparseSegmentV1::EvidencePage(..) | SparseSegmentV1::EvidenceCommit(..) => None,
        })
    }

    /// Publish every new segment in order and return the manifest.
    pub(super) fn replay(
        &self,
        mut publish_segment: impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        for segment in &self.segments {
            publish_segment(segment.publication())?;
        }
        Ok(self.manifest_bytes.clone())
    }
}

/// Everything a sparse build reads besides its parent.
pub(super) struct SparseBuildV1<'a> {
    pub(super) config: &'a CodeIndexProductionConfigV1,
    pub(super) physical_artifacts: &'a SharedPhysicalCodeArtifactPoolV1,
    pub(super) retained_parses: &'a SharedRetainedParsePool,
    pub(super) intake: &'a SanitizedCodeIntake<StaticLanguageRegistry>,
    pub(super) capability: &'a SanitizedSnapshotCapabilityV1,
    pub(super) extractor: &'a TreeSitterExtractor,
    pub(super) chunker: &'a DeterministicCodeChunker,
    pub(super) repository_parse_identity: &'a CodeIndexRepositoryParseIdentityV1,
    pub(super) ignored_source_roster: &'a IgnoredSourceRosterV1,
    pub(super) captured_files: &'a BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>,
    pub(super) target_projection_key: &'a ProjectionKeyV1,
    pub(super) control: &'a dyn CodeIndexExecutionControlV1,
}

fn present_paths(files: &[SanitizedCodeFileV1]) -> BTreeSet<&str> {
    files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .map(|file| file.logical_path.as_str())
        .collect()
}

/// One edited file: its successor snapshot row, and its artifacts before
/// and after the edit.
pub(super) struct EditedFileV1<'s> {
    pub(super) file: &'s SanitizedCodeFileV1,
    pub(super) before: Arc<FileGenerationArtifactsV1>,
    pub(super) after: Arc<FileGenerationArtifactsV1>,
    clone_stats: ClonePayloadBuildStatsV1,
    stale_invalidations: u64,
}

pub(super) fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

pub(super) fn count(value: usize) -> Result<u64, CodeIndexProductionErrorV1> {
    u64::try_from(value).map_err(|_| contract("sparse generation count exceeds u64"))
}

impl SparseBuildV1<'_> {
    /// The successor of `parent` for `snapshot`, or why it must build cold.
    #[tracing::instrument(name = "code_index.build.sparse", level = "trace", skip_all)]
    pub(super) fn build<S: CodeChunkProjectionSink>(
        &self,
        projection_sink: &mut S,
        parent: &SealedParentGenerationV1,
        plan: &GenerationIncrementPlanV1,
        mut manifest: CodeGenerationManifestV1,
        snapshot: &SanitizedCodeSnapshotV1,
    ) -> Result<
        Result<CodeIndexSparseGenerationV1, CodeIndexColdBuildReasonV1>,
        CodeIndexProductionErrorV1,
    > {
        let control = self.control;
        if plan.is_full_rebuild() {
            return Ok(Err(CodeIndexColdBuildReasonV1::FullRebuild));
        }
        if plan.deleted > 0
            || present_paths(&snapshot.files) != present_paths(&parent.snapshot().files)
        {
            return Ok(Err(CodeIndexColdBuildReasonV1::FilesAddedOrRemoved));
        }
        if parent
            .generation_evidence()?
            .projection_request
            .target_projection_key()
            != self.target_projection_key
        {
            return Ok(Err(CodeIndexColdBuildReasonV1::ProjectionKeyChange));
        }
        let parent_rows = parent
            .snapshot()
            .files
            .iter()
            .map(|file| (file.logical_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        // A carried file keeps its parent segment only when its snapshot row
        // is unchanged; any other present file re-extracts as an edit.
        let edited_rows = snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .filter(|file| parent_rows.get(file.logical_path.as_str()) != Some(file))
            .collect::<Vec<_>>();
        let present_count = present_paths(&snapshot.files).len();
        if edited_rows
            .len()
            .saturating_mul(SPARSE_MAX_CHANGED_FILE_SHARE_DENOMINATOR)
            > present_count
        {
            return Ok(Err(CodeIndexColdBuildReasonV1::ChangedShare));
        }
        lexical_page_source::checkpoint(control)?;
        let edited = self.extract_edited(parent, &edited_rows, &manifest)?;
        if edited
            .iter()
            .any(|file| moves_name_lookups(&file.before, &file.after))
        {
            return Ok(Err(CodeIndexColdBuildReasonV1::MovesNameLookups));
        }
        if edited.iter().any(|file| {
            !file.before.artifacts.go_method_sets.is_empty()
                || !file.after.artifacts.go_method_sets.is_empty()
        }) {
            return Ok(Err(CodeIndexColdBuildReasonV1::MovesGoMethodSets));
        }
        lexical_page_source::checkpoint(control)?;

        let generation_id = manifest.generation_id.clone();
        let parent_id = parent.manifest().generation_id.clone();
        let changes = edited_chunk_changes(parent, &edited, &generation_id, present_count)?;
        let parent_full_replay = parent
            .manifest()
            .source_commitments
            .as_ref()
            .ok_or(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable)?
            .full_replay_digest
            .clone();
        manifest.source_commitments = Some(
            CodeGenerationSourceCommitmentsV1::from_changed_chunks(
                Some(&parent_full_replay),
                &changes.set,
            )
            .map_err(|error| contract(&error.to_string()))?,
        );
        manifest.seal.expected_digest =
            expected_seal_digest(&manifest).map_err(|error| contract(&error.to_string()))?;
        let coverage = successor_coverage(
            parent.coverage(),
            parent.snapshot(),
            snapshot,
            edited.iter().map(|file| file.before.as_ref()),
            edited.iter().map(|file| file.after.as_ref()),
        )?;
        let capability = BaseCapabilityEmitter::new(
            registry_for_snapshot(snapshot)?,
            coverage,
            snapshot.sanitization_receipts.clone(),
        )
        .emit(&manifest)
        .map_err(CodeIndexProductionErrorV1::Capability)?;
        let projection = project_for_publication(
            projection_sink,
            projection_request(
                Some(self.target_projection_key.clone()),
                self.target_projection_key.clone(),
                changes.set,
            )?,
        )
        .map_err(CodeIndexProductionErrorV1::Projection)?;
        lexical_page_source::checkpoint(control)?;

        let sealed = SealedSuccessorV1::new(parent, &manifest, snapshot, &edited)?;
        let resolution = sealed.resolve(parent, &edited)?;
        lexical_page_source::checkpoint(control)?;
        let lineage = edited_lineage(&parent_id, &generation_id, &edited)?;
        let carried_symbols = parent
            .statistics()
            .symbol_count
            .checked_sub(count(
                edited
                    .iter()
                    .map(|file| file.before.artifacts.symbols.len())
                    .sum(),
            )?)
            .ok_or_else(|| contract("sealed parent counts fewer symbols than it carries"))?;
        let lineage_prior = (carried_symbols > 0 || !lineage.is_empty()).then_some(&parent_id);

        let mut segments = Vec::new();
        let mut publish = |publication: SealedGenerationSegmentPublicationV1<'_>| {
            segments.push(SparseSegmentV1::of(publication));
            Ok(())
        };
        let file_segments = sealed.file_segments(parent, &edited, &mut publish)?;
        let (file_evidence, cross_file_edges) = sealed.file_evidence(
            parent,
            &edited,
            &resolution,
            &lineage,
            lineage_prior,
            &mut publish,
        )?;
        let code_graph_pages = sealed.graph_pages(parent, &edited, &resolution, &mut publish)?;
        let resolution_index = reseal_resolution_index(
            &parent.resolution_index()?,
            &edited
                .iter()
                .map(|file| file.before.as_ref())
                .collect::<Vec<_>>(),
            &edited
                .iter()
                .map(|file| file.after.as_ref())
                .collect::<Vec<_>>(),
            &mut publish,
        )?;
        let rosters = FileChunkRostersV1::new(
            edited
                .iter()
                .map(|file| {
                    Ok((
                        sealed.key_of(file.file)?,
                        file.after.artifacts.chunks.chunks.as_slice(),
                    ))
                })
                .collect::<Result<Vec<_>, CodeIndexProductionErrorV1>>()?
                .into_iter(),
        );
        let generation_evidence = write_generation_evidence(
            &PartitionedGenerationEvidenceRefV1::new(&projection, &rosters, lineage_prior)?,
            &mut publish,
        )?;
        drop(rosters);
        let statistics = successor_statistics(parent, &edited, &cross_file_edges)?;
        let chunk_policy = edited
            .iter()
            .flat_map(|file| file.after.artifacts.chunks.chunks.iter())
            .fold(
                if changes.carried_chunks > 0 {
                    parent.chunk_policy().clone()
                } else {
                    ChunkPolicyRevisionSummaryV1::Empty
                },
                |census, chunk| census.with_chunks_under(&chunk.sensitivity.policy_revision),
            );
        let manifest_bytes = seal_partitioned_manifest(&PartitionedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &manifest,
            snapshot,
            statistics: &statistics,
            chunk_count: changes.chunk_count,
            chunk_policy: &chunk_policy,
            repository_parse_identity: self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
            file_segments: &file_segments,
            file_evidence: &file_evidence,
            coverage,
            capability: &capability,
            generation_evidence: &generation_evidence,
            code_graph_pages: &code_graph_pages,
            resolution_index: &resolution_index,
        })?;
        let (mut reused, mut computed, mut stale) = (0_u64, 0_u64, 0_u64);
        for file in &edited {
            reused = reused.saturating_add(file.clone_stats.reused);
            computed = computed.saturating_add(file.clone_stats.computed);
            stale = stale.saturating_add(file.stale_invalidations);
        }
        Ok(Ok(CodeIndexSparseGenerationV1 {
            manifest,
            snapshot: snapshot.clone(),
            statistics,
            chunk_count: changes.chunk_count,
            projection,
            segments,
            manifest_bytes,
            clone_payloads_reused: reused,
            clone_payloads_computed: computed,
            clone_stale_invalidations: stale,
        }))
    }

    /// Decode each edited file as the parent sealed it and extract it anew.
    fn extract_edited<'s>(
        &self,
        parent: &SealedParentGenerationV1,
        rows: &[&'s SanitizedCodeFileV1],
        manifest: &CodeGenerationManifestV1,
    ) -> Result<Vec<EditedFileV1<'s>>, CodeIndexProductionErrorV1> {
        let extracted = collect_bounded_ordered(rows, |file| {
            lexical_page_source::checkpoint(self.control)?;
            let before = parent.decode_parent_file(&file.logical_path)?;
            let (reuse_key, after, clone_stats) = extract_file(
                self.config,
                self.physical_artifacts,
                self.retained_parses,
                true,
                self.intake,
                self.capability,
                manifest,
                self.extractor,
                self.chunker,
                self.repository_parse_identity,
                file,
                Some(&before.artifacts.clone_bodies),
                self.captured_files,
                self.control,
            )?;
            let stale_invalidations = before.stale_clone_bindings(&after);
            Ok::<_, CodeIndexProductionErrorV1>((
                reuse_key,
                before,
                after,
                clone_stats,
                stale_invalidations,
            ))
        })?;
        let mut edited = Vec::with_capacity(extracted.len());
        for (file, (reuse_key, before, after, clone_stats, stale_invalidations)) in
            rows.iter().zip(extracted)
        {
            self.physical_artifacts.insert(reuse_key, &after);
            edited.push(EditedFileV1 {
                file,
                before,
                after,
                clone_stats,
                stale_invalidations,
            });
        }
        Ok(edited)
    }
}

/// The edited files' chunk changes and the successor's chunk counts.
struct EditedChunkChangesV1 {
    set: ChangedCodeChunkSetV1,
    chunk_count: u64,
    carried_chunks: u64,
}

fn edited_chunk_changes(
    parent: &SealedParentGenerationV1,
    edited: &[EditedFileV1<'_>],
    generation_id: &CodeGenerationId,
    present_files: usize,
) -> Result<EditedChunkChangesV1, CodeIndexProductionErrorV1> {
    let before = edited
        .iter()
        .flat_map(|file| file.before.artifacts.chunks.chunks.iter())
        .map(|chunk| (&chunk.id, &chunk.content_digest))
        .collect::<BTreeMap<_, _>>();
    let after = edited
        .iter()
        .flat_map(|file| file.after.artifacts.chunks.chunks.iter())
        .map(|chunk| (&chunk.id, &chunk.content_digest))
        .collect::<BTreeMap<_, _>>();
    let mut added_or_changed = Vec::new();
    for (chunk_id, digest) in &after {
        match before.get(chunk_id) {
            Some(prior) if prior == digest => {}
            prior => added_or_changed.push(ChangedCodeChunkV1 {
                chunk_id: (*chunk_id).clone(),
                prior_digest: prior.map(|prior| (*prior).clone()),
                current_digest: Some((*digest).clone()),
            }),
        }
    }
    let deleted = before
        .iter()
        .filter(|(chunk_id, _)| !after.contains_key(*chunk_id))
        .map(|(chunk_id, digest)| ChangedCodeChunkV1 {
            chunk_id: (*chunk_id).clone(),
            prior_digest: Some((*digest).clone()),
            current_digest: None,
        })
        .collect::<Vec<_>>();
    let carried_chunks = parent
        .chunk_count()
        .checked_sub(count(before.len())?)
        .ok_or_else(|| contract("sealed parent counts fewer chunks than its edited files hold"))?;
    let chunk_count = carried_chunks
        .checked_add(count(after.len())?)
        .ok_or_else(|| contract("sparse generation chunk count exceeds u64"))?;
    let reused_count = chunk_count
        .checked_sub(count(added_or_changed.len())?)
        .ok_or_else(|| contract("sparse generation reuses a negative chunk count"))?;
    let parent_full_replay = parent
        .manifest()
        .source_commitments
        .as_ref()
        .ok_or(CodeIndexProductionErrorV1::SourceCommitmentsUnavailable)?
        .full_replay_digest
        .clone();
    let carried_files = present_files.saturating_sub(edited.len());
    let (reused_count, reused_digest) = ChangedCodeChunkSetV1::seal_arc_shared_reused_partition(
        &parent_full_replay,
        &parent.manifest().generation_id,
        generation_id,
        reused_count,
        count(carried_files)?,
    )
    .map_err(|error| contract(&error.to_string()))?;
    let mut set = ChangedCodeChunkSetV1 {
        from_generation: Some(parent.manifest().generation_id.clone()),
        to_generation: generation_id.clone(),
        manifest_digest: crate::generations::placeholder_digest(),
        added_or_changed,
        deleted,
        reused_count,
        reused_digest,
    };
    set.seal().map_err(|error| contract(&error.to_string()))?;
    Ok(EditedChunkChangesV1 {
        set,
        chunk_count,
        carried_chunks,
    })
}

/// Lineage of the edited files' symbols against their parent symbols, in
/// current-occurrence order. Every carried symbol continues unchanged from
/// itself, which sealing leaves implicit.
fn edited_lineage(
    parent_id: &CodeGenerationId,
    generation_id: &CodeGenerationId,
    edited: &[EditedFileV1<'_>],
) -> Result<Vec<SymbolLineageCandidateV1>, CodeIndexProductionErrorV1> {
    let prior = GenerationSymbolIndexV1::new(
        parent_id.clone(),
        edited
            .iter()
            .flat_map(|file| file.before.artifacts.symbols.iter().cloned())
            .collect(),
    )
    .map_err(CodeIndexProductionErrorV1::Lineage)?;
    let current = GenerationSymbolIndexV1::new(
        generation_id.clone(),
        edited
            .iter()
            .flat_map(|file| file.after.artifacts.symbols.iter().cloned())
            .collect(),
    )
    .map_err(CodeIndexProductionErrorV1::Lineage)?;
    let fresh = current
        .symbols
        .iter()
        .map(Arc::as_ptr)
        .collect::<HashSet<_>>();
    SymbolLineageResolver::new()
        .resolve_fresh_symbol_ptrs(&prior, &current, &fresh)
        .map_err(CodeIndexProductionErrorV1::Lineage)
}

/// Statistics move by the replaced files' terms: their source bytes and
/// symbols, their own edges, and the cross-file edges sealed from every file
/// whose evidence the successor re-decided.
fn successor_statistics(
    parent: &SealedParentGenerationV1,
    edited: &[EditedFileV1<'_>],
    cross_file_edges: &CrossFileEdgeCountsV1,
) -> Result<CodeIndexGenerationStatisticsV1, CodeIndexProductionErrorV1> {
    let source_bytes = |file: &FileGenerationArtifactsV1| {
        let coverage = &file.extraction.coverage;
        coverage
            .parsed_bytes
            .checked_add(coverage.error_bytes)
            .and_then(|total| total.checked_add(coverage.unsupported_bytes))
            .ok_or_else(|| contract("generation file coverage byte total overflowed"))
    };
    let shrink = |total: u64, removed: u64| {
        total.checked_sub(removed).ok_or_else(|| {
            contract("sealed parent statistics do not contain the files its successor replaces")
        })
    };
    let grow = |total: u64, added: u64| {
        total
            .checked_add(added)
            .ok_or_else(|| contract("sparse generation statistics overflowed"))
    };
    let parent = parent.statistics();
    let (mut source_total_bytes, mut symbol_count, mut edge_count) = (
        parent.source_total_bytes,
        parent.symbol_count,
        parent.edge_count,
    );
    for file in edited {
        source_total_bytes = grow(
            shrink(source_total_bytes, source_bytes(&file.before)?)?,
            source_bytes(&file.after)?,
        )?;
        symbol_count = grow(
            shrink(symbol_count, count(file.before.artifacts.symbols.len())?)?,
            count(file.after.artifacts.symbols.len())?,
        )?;
        edge_count = grow(
            shrink(edge_count, count(file.before.artifacts.edges.len())?)?,
            count(file.after.artifacts.edges.len())?,
        )?;
    }
    edge_count = grow(
        shrink(edge_count, cross_file_edges.before)?,
        cross_file_edges.after,
    )?;
    Ok(CodeIndexGenerationStatisticsV1 {
        source_total_bytes,
        symbol_count,
        edge_count,
    })
}

/// The digest determinism checks compare: the successor's content identity,
/// its file segments, and its graph pages, which hold every edge. Cold and
/// sparse seals of one tree agree on it.
pub(super) fn lane_digest(
    snapshot: &SanitizedCodeSnapshotV1,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
    pages: &[PartitionedCodeGraphPageDescriptorV1],
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.code-index-lane.v1\0");
    hasher.update(snapshot.content_identity.as_str().as_bytes());
    for segment in file_segments {
        hasher.update(b"\0f");
        hasher.update(segment.segment_digest.as_str().as_bytes());
    }
    for page in pages {
        hasher.update(b"\0p");
        hasher.update(page.page_digest.as_str().as_bytes());
    }
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| contract(&error.to_string()))
}
