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
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

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
use super::sparse_successor::{CrossFileEdgeCountsV1, SealedSuccessorV1, SuccessorResolutionV1};
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
    pub(super) construction_operation: &'a ResidentMemoryOperationV1,
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

/// Charge the same shared chunk and symbol allocations once across both
/// sides of the edited set, using the canonical owned-row accounting.
fn edited_artifact_bytes(edited: &[EditedFileV1<'_>]) -> usize {
    let mut files = HashSet::new();
    let mut chunks = HashSet::new();
    let mut symbols = HashSet::new();
    let mut bytes = edited
        .len()
        .saturating_mul(std::mem::size_of::<EditedFileV1<'_>>());
    for file in edited.iter().flat_map(|file| [&file.before, &file.after]) {
        if files.insert(Arc::as_ptr(file)) {
            bytes = bytes.saturating_add(super::resident_bytes::file_bytes(file));
        }
        for chunk in &file.artifacts.chunks.chunks {
            if chunks.insert(Arc::as_ptr(chunk)) {
                bytes = bytes.saturating_add(super::resident_bytes::chunk_bytes(chunk));
            }
        }
        for symbol in &file.artifacts.symbols {
            if symbols.insert(Arc::as_ptr(symbol)) {
                bytes = bytes.saturating_add(super::resident_bytes::symbol_bytes(symbol));
            }
        }
    }
    bytes
}

impl SparseBuildV1<'_> {
    /// The successor of `parent` for `snapshot`, or why it must build cold.
    #[tracing::instrument(name = "code_index.build.sparse", level = "trace", skip_all)]
    pub(super) fn build(
        &self,
        parent: &SealedParentGenerationV1,
        plan: &GenerationIncrementPlanV1,
        mut manifest: CodeGenerationManifestV1,
        snapshot: &SanitizedCodeSnapshotV1,
        preparation: &mut impl CodeIndexPreparationPort,
    ) -> Result<
        Result<CodeIndexSealedPublicationV1, CodeIndexColdBuildReasonV1>,
        CodeIndexProductionErrorV1,
    > {
        let control = self.control;
        let mut memory = ConstructionMemoryV1::new(self.construction_operation.clone())?;
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
        let artifact_bytes = edited_artifact_bytes(&edited);
        memory.resize(count(artifact_bytes)?)?;
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
        let changes = edited_chunk_changes(
            parent,
            &edited,
            &generation_id,
            present_count,
            &mut |change_bytes| {
                memory.resize(
                    count(artifact_bytes)?
                        .checked_add(change_bytes)
                        .ok_or_else(|| contract("sparse construction charge exceeds u64"))?,
                )
            },
        )?;
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
        let sealed = SealedSuccessorV1::new(parent, &manifest, snapshot, &edited)?;
        let resolution = sealed.resolve(parent, &edited, self.construction_operation)?;
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

        let mut publish = |publication: SealedGenerationSegmentPublicationV1<'_>| {
            preparation
                .write_segment(publication)
                .map_err(CodeIndexProductionErrorV1::Publication)
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
        let statistics = successor_statistics(parent, &edited, &cross_file_edges, &resolution)?;
        drop(resolution);
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
        let projection = project_for_publication(projection_request(
            Some(self.target_projection_key.clone()),
            self.target_projection_key.clone(),
            changes.set,
        )?)
        .map_err(CodeIndexProductionErrorV1::Projection)?;
        memory.resize(count(artifact_bytes.saturating_add(
            super::resident_bytes::projection_heap_bytes(&projection),
        ))?)?;
        lexical_page_source::checkpoint(control)?;

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
        let mut publication = CodeIndexSealedPublicationV1::from_prepared(
            Arc::from(manifest_bytes),
            projection,
            None,
            (reused, computed, stale),
        )?;
        drop(edited);
        memory.resize(count(super::resident_bytes::sealed_publication_bytes(
            &publication,
            super::resident_bytes::chunk_policy_heap_bytes(&chunk_policy),
        ))?)?;
        publication.construction_memory = Some(Arc::new(memory));
        Ok(Ok(publication))
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
    retain_changes: &mut impl FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
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
    let changed_bytes = super::resident_bytes::vec_bytes(
        &set.added_or_changed,
        set.added_or_changed.capacity(),
        super::resident_bytes::changed_chunk_heap_bytes,
    )
    .saturating_add(super::resident_bytes::vec_bytes(
        &set.deleted,
        set.deleted.capacity(),
        super::resident_bytes::changed_chunk_heap_bytes,
    ));
    retain_changes(count(changed_bytes)?)?;
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
    resolution: &SuccessorResolutionV1,
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
        ambiguous_name_drops: parent
            .ambiguous_name_drops
            .map(|total| {
                grow(
                    shrink(total, resolution.ambiguous_before)?,
                    resolution.ambiguous_after,
                )
            })
            .transpose()?,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::EditedFileV1;
    use crate::clones::ClonePayloadBuildStatsV1;
    use crate::production::sealed_parent::SealedParentGenerationV1;
    use crate::production::sparse_increment_tests::{owner, request, scope};
    use crate::production::sparse_successor::SealedSuccessorV1;
    use crate::production::{MemorySealedPublicationStoreV1, UninterruptibleCodeIndexControlV1};

    #[test]
    fn sparse_resolved_rows_keep_their_operation_reservation_through_sealing() {
        let store = MemorySealedPublicationStoreV1::default();
        let mut owner = owner(&store);
        let seed_request = request(
            &[
                ("src/a.rs", "rust", "pub fn run() { crate::b::helper(); }\n"),
                ("src/b.rs", "rust", "pub fn helper() {}\n"),
                ("src/lib.rs", "rust", "pub mod a; pub mod b;\n"),
            ],
            1_000_000,
        );
        let memory = Arc::clone(&seed_request.resident_memory);
        let published = owner
            .build_and_publish(seed_request, &UninterruptibleCodeIndexControlV1)
            .unwrap();
        let prior = store.decode_active(&scope()).unwrap().unwrap();
        let parent = SealedParentGenerationV1::open(published.sealed_generation().clone()).unwrap();
        let mut successor_request = request(
            &[
                (
                    "src/a.rs",
                    "rust",
                    "pub fn run_again() { crate::b::helper(); }\n",
                ),
                ("src/b.rs", "rust", "pub fn helper() {}\n"),
                ("src/lib.rs", "rust", "pub mod a; pub mod b;\n"),
            ],
            2_000_000,
        );
        successor_request.resident_memory = Arc::clone(&memory);
        let successor_publication = owner
            .build_and_publish(successor_request, &UninterruptibleCodeIndexControlV1)
            .unwrap();
        let decoded = store.decode_active(&scope()).unwrap().unwrap();
        assert_eq!(
            decoded.manifest.generation_id,
            successor_publication.publication().manifest().generation_id,
        );
        let file = decoded
            .snapshot
            .files
            .iter()
            .find(|file| file.logical_path == "src/a.rs")
            .unwrap();
        let artifact = decoded
            .files
            .iter()
            .find(|file| file.authority.logical_path == "src/a.rs")
            .unwrap();
        let edited = vec![EditedFileV1 {
            file,
            before: Arc::clone(
                prior
                    .files
                    .iter()
                    .find(|file| file.authority.logical_path == "src/a.rs")
                    .unwrap(),
            ),
            after: Arc::clone(artifact),
            clone_stats: ClonePayloadBuildStatsV1::default(),
            stale_invalidations: 0,
        }];
        let successor =
            SealedSuccessorV1::new(&parent, &decoded.manifest, &decoded.snapshot, &edited).unwrap();
        let operation = memory.begin_operation();
        let before = memory.snapshot().used_bytes;
        let resolution = successor.resolve(&parent, &edited, &operation).unwrap();
        assert!(resolution.files.values().any(|file| !file.edges.is_empty()));
        let held = resolution._output_memory.as_ref().unwrap().charged_bytes;
        assert!(held > 0);
        assert_eq!(memory.snapshot().used_bytes, before + held);
        let mut staged = 0;
        successor
            .graph_pages(&parent, &edited, &resolution, &mut |_| {
                assert_eq!(memory.snapshot().used_bytes, before + held);
                staged += 1;
                Ok(())
            })
            .unwrap();
        assert!(staged > 0);
        assert_eq!(
            memory.snapshot().used_bytes,
            before + held,
            "sealing must retain the resolution reservation"
        );
        drop(resolution);
        assert_eq!(memory.snapshot().used_bytes, before);
    }
}
