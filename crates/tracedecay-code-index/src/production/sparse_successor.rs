//! The successor's sealed parts, rebuilt only where an edit reaches.
//!
//! [`SealedSuccessorV1`] reads the successor's file set against its parent:
//! it re-decides the edited files' resolution over a view that decodes a
//! carried file only when a lookup lands in it, then seals each present
//! file's segment, evidence, and graph page, encoding anew what the edit or
//! its resolution touched and rekeying every other parent descriptor.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeGenerationManifestV1, FileOccurrenceId,
    ManifestDigest, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1,
    SymbolIdentityDigest,
};

use super::file_evidence_rows::{
    PersistedFileEvidenceV1, compact_one_file_evidence, identity_lineage,
};
use super::graph_pages::{CodeGraphPageInputsV1, owned_occurrences, seal_code_graph_page};
use super::helpers::edge_order;
use super::partitioned_codec::{
    PartitionedCodeGraphPageDescriptorV1, PartitionedFileEvidenceDescriptorV1,
    PartitionedFileSegmentDescriptorV1, SealedGenerationSegmentPublicationV1,
    encode_file_evidence_segment, encode_file_segment, publish_code_graph_page, snapshot_file_keys,
};
use super::resolution_view::ResolutionFileFactsV1;
use super::resolution_view::ResolutionMemoryChangeV1;
use super::sealed_codec::FileScopeIdentityV1;
use super::sealed_parent::{SealedParentGenerationV1, SparseFileSourceV1, SparseFileV1};
use super::sparse_increment::{EditedFileV1, contract, count};
use super::sparse_resolution::{
    ResolvedFileV1, SparseDefinitionWindowV1, SparseSymbolsByNameV1, resolve_edit,
};
use super::{CodeIndexProductionErrorV1, FileGenerationArtifactsV1};
use crate::lineage::SymbolLineageCandidateV1;

/// Cross-file edges the re-decided files sealed in the parent and seal now.
#[derive(Default)]
pub(super) struct CrossFileEdgeCountsV1 {
    pub(super) before: u64,
    pub(super) after: u64,
}

/// The successor's file set as sealing reads it.
pub(super) struct SealedSuccessorV1<'p> {
    snapshot: &'p SanitizedCodeSnapshotV1,
    generation_id: &'p CodeGenerationId,
    snapshot_digest: &'p ManifestDigest,
    scope: FileScopeIdentityV1,
    /// Present files in file-occurrence order, the order resolution indexes.
    present: Vec<&'p SanitizedCodeFileV1>,
    keys: HashMap<&'p FileOccurrenceId, u32>,
    index_of_path: HashMap<&'p str, usize>,
    occurrence_of_path: HashMap<&'p str, &'p FileOccurrenceId>,
    parent_key_of_path: HashMap<&'p str, u32>,
    edited_index: HashMap<&'p str, usize>,
}

impl<'p> SealedSuccessorV1<'p> {
    pub(super) fn new(
        parent: &'p SealedParentGenerationV1,
        manifest: &'p CodeGenerationManifestV1,
        snapshot: &'p SanitizedCodeSnapshotV1,
        edited: &[EditedFileV1<'p>],
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let keys = snapshot_file_keys(snapshot.files.iter().map(|file| &file.file_occurrence_id))?;
        let mut present = snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .collect::<Vec<_>>();
        present.sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        let index_of_path = present
            .iter()
            .enumerate()
            .map(|(index, file)| (file.logical_path.as_str(), index))
            .collect::<HashMap<_, _>>();
        let occurrence_of_path = present
            .iter()
            .map(|file| (file.logical_path.as_str(), &file.file_occurrence_id))
            .collect();
        let parent_key_of_path = parent
            .generation()
            .file_segments
            .iter()
            .map(|descriptor| {
                parent
                    .snapshot()
                    .files
                    .get(descriptor.file_key as usize)
                    .map(|file| (file.logical_path.as_str(), descriptor.file_key))
                    .ok_or_else(|| contract("sealed file segment is outside its snapshot"))
            })
            .collect::<Result<_, _>>()?;
        let edited_index = edited
            .iter()
            .map(|file| {
                index_of_path
                    .get(file.file.logical_path.as_str())
                    .map(|index| (file.file.logical_path.as_str(), *index))
                    .ok_or_else(|| contract("an edited file is not present in its successor"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            snapshot,
            generation_id: &manifest.generation_id,
            snapshot_digest: &manifest.snapshot_digest,
            scope: FileScopeIdentityV1::of(manifest, snapshot),
            present,
            keys,
            index_of_path,
            occurrence_of_path,
            parent_key_of_path,
            edited_index,
        })
    }

    pub(super) fn key_of(
        &self,
        file: &SanitizedCodeFileV1,
    ) -> Result<u32, CodeIndexProductionErrorV1> {
        self.keys
            .get(&file.file_occurrence_id)
            .copied()
            .ok_or_else(|| contract("a successor file is absent from its snapshot"))
    }

    /// The successor's present files for resolution: edited files decoded,
    /// every other file decoded from its parent segment on first read.
    fn view<'v>(
        &'v self,
        parent: &'v SealedParentGenerationV1,
        edited: &'v [EditedFileV1<'p>],
        before: bool,
        construction_operation: &ResidentMemoryOperationV1,
    ) -> Result<Vec<SparseFileV1<'v>>, CodeIndexProductionErrorV1> {
        let edited_by_path = edited
            .iter()
            .map(|file| {
                (
                    file.file.logical_path.as_str(),
                    if before { &file.before } else { &file.after },
                )
            })
            .collect::<HashMap<_, _>>();
        self.present
            .iter()
            .map(|file| {
                let language = file
                    .language
                    .as_ref()
                    .map(|language| language.as_str())
                    .ok_or_else(|| contract("present snapshot file has no declared language"))?;
                Ok(match edited_by_path.get(file.logical_path.as_str()) {
                    Some(after) => SparseFileV1::decoded(
                        &file.logical_path,
                        language,
                        Arc::clone(after),
                        construction_operation.clone(),
                    )?,
                    None => SparseFileV1::carried(
                        &file.logical_path,
                        language,
                        SparseFileSourceV1 {
                            parent,
                            descriptor: parent
                                .file_segment(&file.logical_path)
                                .ok_or_else(|| contract("a carried file has no parent segment"))?,
                            file_occurrence_id: &file.file_occurrence_id,
                            generation_id: self.generation_id,
                            snapshot_digest: self.snapshot_digest,
                            construction_operation: construction_operation.clone(),
                        },
                    ),
                })
            })
            .collect()
    }

    /// Re-decide the edited files' sites; a file whose resolution decoded
    /// a carried file is reported in `decoded`.
    pub(super) fn resolve(
        &self,
        parent: &SealedParentGenerationV1,
        edited: &[EditedFileV1<'p>],
        construction_operation: &ResidentMemoryOperationV1,
    ) -> Result<SuccessorResolutionV1, CodeIndexProductionErrorV1> {
        if edited.is_empty() {
            return Ok(SuccessorResolutionV1::default());
        }
        let view = self.view(parent, edited, false, construction_operation)?;
        let before_view = self.view(parent, edited, true, construction_operation)?;
        let index = parent.resolution_index()?;
        let window = SparseDefinitionWindowV1::new(&index, construction_operation)?;
        let before_by_name = SparseSymbolsByNameV1::new(
            &index,
            &self.index_of_path,
            &window,
            std::iter::empty(),
            construction_operation,
        )?;
        let by_name = SparseSymbolsByNameV1::new(
            &index,
            &self.index_of_path,
            &window,
            edited.iter().map(|file| {
                (
                    self.edited_index[file.file.logical_path.as_str()],
                    file.file.logical_path.as_str(),
                    &file.after,
                )
            }),
            construction_operation,
        )?;
        let pairs = edited
            .iter()
            .map(|file| {
                (
                    self.edited_index[file.file.logical_path.as_str()],
                    Arc::clone(&file.before),
                    Arc::clone(&file.after),
                )
            })
            .collect::<Vec<_>>();
        let resolution = resolve_edit(
            parent,
            &index,
            &view,
            &by_name,
            &before_view,
            &before_by_name,
            &window,
            &pairs,
            &self.index_of_path,
            &self.occurrence_of_path,
            &self.parent_key_of_path,
        );
        let resolution = resolution?;
        Ok(SuccessorResolutionV1 {
            files: resolution.files,
            ambiguous_before: resolution.ambiguous_before,
            ambiguous_after: resolution.ambiguous_after,
            _output_memory: Some(resolution.output_memory),
        })
    }

    /// Every present file's segment descriptor: the edited files encoded
    /// anew, every other file's parent descriptor rekeyed.
    pub(super) fn file_segments(
        &self,
        parent: &SealedParentGenerationV1,
        edited: &[EditedFileV1<'_>],
        publish: &mut impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<PartitionedFileSegmentDescriptorV1>, CodeIndexProductionErrorV1> {
        let edited_by_path = edited
            .iter()
            .map(|file| (file.file.logical_path.as_str(), &file.after))
            .collect::<HashMap<_, _>>();
        let mut descriptors = Vec::with_capacity(self.present.len());
        for (key, file) in self.snapshot.files.iter().enumerate() {
            if file.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            let key = u32::try_from(key)
                .map_err(|_| contract("sealed generation file key exceeds u32"))?;
            let descriptor = match edited_by_path.get(file.logical_path.as_str()) {
                Some(after) => {
                    let (descriptor, bytes) =
                        encode_file_segment(self.generation_id, &self.scope, after, key)?;
                    publish(SealedGenerationSegmentPublicationV1::File {
                        digest: &descriptor.segment_digest,
                        bytes: &bytes,
                    })?;
                    descriptor
                }
                None => {
                    let parent = parent
                        .file_segment(&file.logical_path)
                        .ok_or_else(|| contract("a carried file has no parent segment"))?;
                    if parent.file_occurrence_id != file.file_occurrence_id {
                        return Err(contract("a carried file changed its occurrence"));
                    }
                    let mut descriptor = parent.clone();
                    descriptor.file_key = key;
                    descriptor
                }
            };
            descriptors.push(descriptor);
        }
        Ok(descriptors)
    }

    /// Every present file's evidence descriptor: the evidence of each file
    /// resolution re-decided and of each edited file sealed anew, a carried
    /// file's explicit parent lineage resealed as identity lineage, and every
    /// other file's parent descriptor rekeyed.
    pub(super) fn file_evidence(
        &self,
        parent: &SealedParentGenerationV1,
        edited: &[EditedFileV1<'_>],
        resolution: &SuccessorResolutionV1,
        lineage: &[SymbolLineageCandidateV1],
        lineage_prior: Option<&CodeGenerationId>,
        publish: &mut impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<
        (
            Vec<PartitionedFileEvidenceDescriptorV1>,
            CrossFileEdgeCountsV1,
        ),
        CodeIndexProductionErrorV1,
    > {
        let edited_by_path = edited
            .iter()
            .map(|file| (file.file.logical_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let mut lineage_by_file =
            HashMap::<&FileOccurrenceId, Vec<&SymbolLineageCandidateV1>>::new();
        for file in edited {
            for candidate in lineage.iter().filter(|candidate| {
                file.after
                    .artifacts
                    .symbols
                    .iter()
                    .any(|symbol| symbol.occurrence == candidate.current_occurrence)
            }) {
                lineage_by_file
                    .entry(&file.file.file_occurrence_id)
                    .or_default()
                    .push(candidate);
            }
        }
        let mut counts = CrossFileEdgeCountsV1::default();
        let mut stored = parent
            .generation()
            .file_evidence
            .iter()
            .map(|descriptor| descriptor.segment_digest.clone())
            .collect::<BTreeSet<_>>();
        let mut descriptors = Vec::new();
        for (key, file) in self.snapshot.files.iter().enumerate() {
            if file.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            let key = u32::try_from(key)
                .map_err(|_| contract("sealed generation file key exceeds u32"))?;
            let parent_key = self
                .parent_key_of_path
                .get(file.logical_path.as_str())
                .copied()
                .ok_or_else(|| contract("a successor file has no parent segment"))?;
            let parent_descriptor = parent.file_evidence_descriptor(parent_key);
            let resolved = self
                .index_of_path
                .get(file.logical_path.as_str())
                .and_then(|index| resolution.files.get(index));
            let edited_file = edited_by_path.get(file.logical_path.as_str());
            let evidence = if resolved.is_some() || edited_file.is_some() {
                if let Some(parent_evidence) = parent.file_evidence(parent_key)? {
                    counts.before = counts
                        .before
                        .saturating_add(count(parent_evidence.cross_file_edge_count())?);
                }
                let artifacts = match (resolved, edited_file) {
                    (_, Some(edited)) => Arc::clone(&edited.after),
                    (Some(_), None) => parent.decode_file(
                        parent
                            .file_segment(&file.logical_path)
                            .ok_or_else(|| contract("a re-decided file has no parent segment"))?,
                        &file.file_occurrence_id,
                        self.generation_id,
                        self.snapshot_digest,
                    )?,
                    (None, None) => return Err(contract("a re-decided file has no artifacts")),
                };
                let edges = resolved.map_or_default(|resolved| {
                    resolved
                        .edges
                        .iter()
                        .map(|(edge, path, identity)| (edge, path.as_str(), identity))
                        .collect::<Vec<_>>()
                });
                counts.after = counts.after.saturating_add(count(edges.len())?);
                let calls = resolved.map_or_default(|resolved| {
                    resolved.unresolved_calls.iter().collect::<Vec<_>>()
                });
                let identity;
                let file_lineage = match edited_file {
                    Some(_) => lineage_by_file
                        .remove(&file.file_occurrence_id)
                        .unwrap_or_default(),
                    None => {
                        identity = identity_lineage(&artifacts, lineage_prior, self.generation_id)?;
                        identity.iter().collect()
                    }
                };
                Some(compact_one_file_evidence(
                    &artifacts,
                    &edges,
                    &calls,
                    &file_lineage,
                    lineage_prior,
                    self.generation_id,
                )?)
            } else if parent_descriptor.is_some_and(|descriptor| descriptor.explicit_lineage) {
                parent
                    .file_evidence(parent_key)?
                    .map(PersistedFileEvidenceV1::with_identity_lineage)
            } else {
                if let Some(descriptor) = parent_descriptor {
                    if descriptor.file_occurrence_id != file.file_occurrence_id {
                        return Err(contract("a carried file changed its occurrence"));
                    }
                    let mut descriptor = descriptor.clone();
                    descriptor.file_key = key;
                    descriptors.push(descriptor);
                }
                continue;
            };
            let Some(evidence) = evidence.filter(|evidence| !evidence.is_empty()) else {
                continue;
            };
            let (descriptor, bytes) =
                encode_file_evidence_segment(key, &file.file_occurrence_id, &evidence)?;
            if stored.insert(descriptor.segment_digest.clone()) {
                publish(SealedGenerationSegmentPublicationV1::FileEvidence {
                    digest: &descriptor.segment_digest,
                    bytes: &bytes,
                })?;
            }
            descriptors.push(descriptor);
        }
        Ok((descriptors, counts))
    }

    /// Every snapshot file's graph page: the pages of edited and re-decided
    /// files and of rows the snapshot changed rebuilt, every other page's
    /// parent descriptor rekeyed.
    pub(super) fn graph_pages(
        &self,
        parent: &SealedParentGenerationV1,
        edited: &[EditedFileV1<'_>],
        resolution: &SuccessorResolutionV1,
        publish: &mut impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<PartitionedCodeGraphPageDescriptorV1>, CodeIndexProductionErrorV1> {
        let parent_pages = parent.graph_pages_by_path();
        let parent_rows = parent
            .snapshot()
            .files
            .iter()
            .map(|file| (file.logical_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let reusable = parent
            .generation()
            .code_graph_pages
            .iter()
            .map(|page| page.page_digest.clone())
            .collect::<BTreeSet<_>>();
        let edited_by_path = edited
            .iter()
            .map(|file| (file.file.logical_path.as_str(), &file.after))
            .collect::<HashMap<_, _>>();
        let files_by_occurrence = self
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, file))
            .collect::<BTreeMap<_, _>>();
        let mut pages = Vec::with_capacity(self.snapshot.files.len());
        for (key, file) in self.snapshot.files.iter().enumerate() {
            let key =
                u32::try_from(key).map_err(|_| contract("code graph page file key exceeds u32"))?;
            let path = file.logical_path.as_str();
            let resolved = self
                .index_of_path
                .get(file.logical_path.as_str())
                .and_then(|index| resolution.files.get(index));
            let edited_file = edited_by_path.get(path);
            let row_unchanged = parent_rows.get(path) == Some(&file);
            if resolved.is_none() && edited_file.is_none() && row_unchanged {
                let parent_page = parent_pages
                    .get(path)
                    .ok_or_else(|| contract("a carried file has no parent graph page"))?;
                let mut descriptor = (*parent_page).clone();
                descriptor.file_key = key;
                pages.push(descriptor);
                continue;
            }
            let artifacts = match (edited_file, resolved) {
                (Some(after), _) => Some(Arc::clone(after)),
                (None, Some(_)) => Some(
                    parent.decode_file(
                        parent
                            .file_segment(&file.logical_path)
                            .ok_or_else(|| contract("a re-decided file has no parent segment"))?,
                        &file.file_occurrence_id,
                        self.generation_id,
                        self.snapshot_digest,
                    )?,
                ),
                (None, None) if file.disposition == SnapshotFileDispositionV1::Present => {
                    return Err(contract(
                        "a present file's graph page changed without its artifacts",
                    ));
                }
                (None, None) => None,
            };
            let inputs = match artifacts.as_deref() {
                Some(artifacts) => {
                    let cross_file = resolved.map_or_default(|resolved| resolved.edges.as_slice());
                    self.page_inputs(
                        key,
                        file,
                        artifacts,
                        cross_file,
                        resolved.map_or_default(|resolved| resolved.unresolved_calls.clone()),
                    )?
                }
                None => CodeGraphPageInputsV1 {
                    file_key: key,
                    snapshot_file: file,
                    artifacts: None,
                    edges: Vec::new(),
                    unresolved_calls: Vec::new(),
                    target_files: BTreeMap::new(),
                    placeholder_targets: BTreeSet::new(),
                    owned_placeholders: BTreeSet::new(),
                },
            };
            let page = seal_code_graph_page(inputs, &files_by_occurrence, self.generation_id)?;
            pages.push(publish_code_graph_page(page, &reusable, publish)?);
        }
        Ok(pages)
    }

    /// One present file's page inputs: its own edges and its cross-file
    /// edges in canonical order, cross-file targets owned by their files,
    /// and its own edges' unowned targets as placeholders it owns.
    fn page_inputs<'f>(
        &self,
        file_key: u32,
        snapshot_file: &'f SanitizedCodeFileV1,
        artifacts: &'f FileGenerationArtifactsV1,
        cross_file: &[(CanonicalRelationEdgeV1, String, SymbolIdentityDigest)],
        unresolved_calls: Vec<crate::chunks::CodeIndexUnresolvedReferenceV1>,
    ) -> Result<CodeGraphPageInputsV1<'f>, CodeIndexProductionErrorV1> {
        let owned = owned_occurrences(artifacts);
        let mut edges = artifacts.artifacts.edges.clone();
        let mut target_files = BTreeMap::new();
        for (edge, path, _) in cross_file {
            if !owned.contains(&edge.to_occurrence) {
                let target = self.occurrence_of_path.get(path.as_str()).ok_or_else(|| {
                    contract("a cross-file edge targets a file outside its successor")
                })?;
                target_files.insert(edge.to_occurrence.clone(), (*target).clone());
            }
            edges.push(edge.clone());
        }
        edges.sort_by(edge_order);
        let placeholder_targets = artifacts
            .artifacts
            .edges
            .iter()
            .filter(|edge| !owned.contains(&edge.to_occurrence))
            .map(|edge| edge.to_occurrence.clone())
            .collect::<BTreeSet<_>>();
        Ok(CodeGraphPageInputsV1 {
            file_key,
            snapshot_file,
            artifacts: Some(artifacts),
            edges,
            unresolved_calls,
            target_files,
            owned_placeholders: placeholder_targets.clone(),
            placeholder_targets,
        })
    }
}

/// The files whose cross-file evidence the edit re-decided, by present-file
/// index. The reservation outlives every retained row through sealing.
#[derive(Default)]
pub(super) struct SuccessorResolutionV1 {
    pub(super) files: BTreeMap<usize, ResolvedFileV1>,
    pub(super) ambiguous_before: u64,
    pub(super) ambiguous_after: u64,
    // Declared last so rows are destroyed before their reservation is released.
    pub(super) _output_memory: Option<super::ConstructionMemoryV1>,
}

/// Canonical sealed outputs of cold resolution. No decoded file, symbol index,
/// request, or projection receipt escapes this operation.
pub(super) struct StagedResolutionPartsV1 {
    pub(super) file_evidence: Vec<PartitionedFileEvidenceDescriptorV1>,
    pub(super) code_graph_pages: Vec<PartitionedCodeGraphPageDescriptorV1>,
    pub(super) resolution_index: super::resolution_index::PartitionedResolutionIndexDescriptorV1,
    pub(super) coverage: tracedecay_domain::CoverageSummaryV1,
    pub(super) statistics: super::CodeIndexGenerationStatisticsV1,
}

/// The canonical retained representation at a resolution boundary. This is
/// emitted only at phase completion or failure, never once per file.
fn trace_cold_resolution_facts(
    facts: &[super::resolution_view::CompactFileResolutionV1],
    retained: usize,
    ordered_bytes: usize,
    completed: bool,
) {
    tracing::trace!(
        phase = "facts",
        completed,
        loaded_files = facts.len(),
        retained_facts_bytes = retained,
        ordered_bytes,
        symbol_count = facts.iter().map(|file| file.symbols().len()).sum::<usize>(),
        retained_symbol_records_bytes = facts
            .iter()
            .map(|file| file.owned_symbol_bytes())
            .sum::<usize>(),
        retained_rows_bytes = facts
            .iter()
            .map(|file| file.retained_row_bytes())
            .sum::<usize>(),
        graph_owner_count = facts
            .iter()
            .map(|file| file.graph_owners().len())
            .sum::<usize>(),
        retained_graph_owner_bytes = facts
            .iter()
            .map(|file| file.graph_owner_bytes())
            .sum::<usize>(),
        "cold resolution facts boundary"
    );
}

/// Resolve one real cold generation from its already staged canonical files.
/// Only charged binding facts span files; decode and encoding pages are
/// released before the next file operation, and all facts drop before return.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_staged_generation(
    manifest: &CodeGenerationManifestV1,
    snapshot: &SanitizedCodeSnapshotV1,
    files: &[PartitionedFileSegmentDescriptorV1],
    read: Arc<super::SealedSegmentReaderV1>,
    control: &dyn super::CodeIndexExecutionControlV1,
    retain_facts: &mut impl FnMut(u64) -> Result<(), CodeIndexProductionErrorV1>,
    publish: &mut impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<StagedResolutionPartsV1, CodeIndexProductionErrorV1> {
    let facts_span = tracing::trace_span!("code_index.cold.facts", files = files.len()).entered();
    let source = super::sealed_parent::StagedFileSourceV1::new(manifest, snapshot, files, read)?;
    // The full builder indexes files in occurrence order; retain that order
    // even when snapshot keys have a different order.
    let ordered_bytes = files
        .len()
        .checked_mul(std::mem::size_of::<&PartitionedFileSegmentDescriptorV1>())
        .ok_or_else(|| contract("resolution file order bytes overflowed"))?;
    let mut retained = files
        .len()
        .checked_mul(std::mem::size_of::<
            super::resolution_view::CompactFileResolutionV1,
        >())
        .ok_or_else(|| contract("resolution facts vector bytes overflowed"))?;
    let outer_bytes = retained
        .checked_add(ordered_bytes)
        .ok_or_else(|| contract("resolution outer vector bytes overflowed"))?;
    let _ordered_layout = std::alloc::Layout::array::<&PartitionedFileSegmentDescriptorV1>(
        files.len(),
    )
    .map_err(|error| contract(&format!("resolution file order layout is invalid: {error}")))?;
    let _facts_layout =
        std::alloc::Layout::array::<super::resolution_view::CompactFileResolutionV1>(files.len())
            .map_err(|error| contract(&format!("resolution facts layout is invalid: {error}")))?;
    retain_facts(count(outer_bytes)?)?;
    let mut ordered = files.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
    let mut facts = Vec::with_capacity(files.len());
    let mut coverage = super::helpers::coverage_summary(snapshot, &[]);
    let mut source_total_bytes = 0_u64;
    let mut symbol_count = 0_u64;
    let facts_loaded = (|| -> Result<(), CodeIndexProductionErrorV1> {
        for descriptor in &ordered {
            super::lexical_page_source::checkpoint(control)?;
            let file = source.decode_file(descriptor.file_key)?;
            coverage = super::helpers::add_coverage(coverage, super::helpers::file_coverage(&file));
            let bytes = super::generation_statistics::checked_source_total(std::iter::once(
                &file.extraction.coverage,
            ))?;
            source_total_bytes = source_total_bytes
                .checked_add(bytes)
                .ok_or_else(|| contract("generation source byte total overflowed"))?;
            symbol_count = symbol_count
                .checked_add(count(file.artifacts.symbols.len())?)
                .ok_or_else(|| contract("generation symbol count exceeds u64"))?;
            let source_bytes = super::resolution_view::resolution_source_bytes(&file)?;
            let base = retained
                .checked_add(ordered_bytes)
                .and_then(|bytes| bytes.checked_add(source_bytes))
                .ok_or_else(|| contract("resolution source overlap exceeds usize"))?;
            let mut admitted = 0_u64;
            let compact =
                super::resolution_view::CompactFileResolutionV1::from_file(&file, &mut |copies| {
                    // The outer facts Vec already owns the compact value's inline slot.
                    let copied_heap = copies
                        .checked_sub(std::mem::size_of::<
                            super::resolution_view::CompactFileResolutionV1,
                        >() as u64)
                        .ok_or_else(|| contract("resolution clone lacks its inline body"))?;
                    let overlap = count(base)?
                        .checked_add(copied_heap)
                        .ok_or_else(|| contract("resolution source/clone overlap exceeds u64"))?;
                    retain_facts(overlap).inspect_err(|_| {
                        tracing::trace!(
                            phase = "facts",
                            retained_facts_bytes = retained,
                            ordered_bytes,
                            current_source_bytes = source_bytes,
                            current_copy_bound_bytes = copied_heap,
                            requested_overlap_bytes = overlap,
                            "cold fact window admission refused"
                        );
                    })?;
                    admitted = overlap;
                    Ok(())
                })?;
            drop(file);
            retained = retained
                .checked_add(
                    compact
                        .retained_bytes()
                        .checked_sub(std::mem::size_of::<
                            super::resolution_view::CompactFileResolutionV1,
                        >())
                        .ok_or_else(|| contract("resolution facts lack their inline body"))?,
                )
                .ok_or_else(|| contract("resolution facts bytes overflowed"))?;
            let settled = count(
                retained
                    .checked_add(ordered_bytes)
                    .ok_or_else(|| contract("resolution facts/order bytes overflowed"))?,
            )?;
            if settled > admitted {
                return Err(contract(
                    "resolution facts exceed their admitted source/clone bound",
                ));
            }
            facts.push(compact);
            retain_facts(settled)?;
        }
        Ok(())
    })();
    trace_cold_resolution_facts(&facts, retained, ordered_bytes, facts_loaded.is_ok());
    facts_loaded?;
    drop(facts_span);
    super::lexical_page_source::checkpoint(control)?;
    let facts_bytes = retained.saturating_add(
        ordered
            .capacity()
            .saturating_mul(std::mem::size_of::<&PartitionedFileSegmentDescriptorV1>()),
    );
    let cross_span = tracing::trace_span!(
        "code_index.cold.cross_resolution",
        retained_facts_bytes = facts_bytes,
    )
    .entered();
    tracing::trace!(
        phase = "cross_resolution",
        retained_facts_bytes = facts_bytes,
        "cold resolution phase started"
    );
    let resolved = super::helpers::resolve_cross_file_references_charged(&facts, &mut |bytes| {
        retain_facts(count(facts_bytes)?.saturating_add(bytes))
    })?;
    let super::helpers::CrossFileResolutionV1 {
        mut edges,
        gaps,
        ambiguous_name_drops,
    } = resolved;
    let cross_count = edges.len();
    let own_count = facts.iter().map(|file| file.edges().len()).sum::<usize>();
    let mut outputs = super::resident_bytes::vec_bytes(
        &edges,
        edges.capacity(),
        super::resident_bytes::edge_heap_bytes,
    )
    .saturating_add(super::resident_bytes::vec_bytes(
        &gaps,
        gaps.capacity(),
        super::resident_bytes::unresolved_heap_bytes,
    ));
    super::resolution_view::reserve_resolution_vec(
        &mut edges,
        own_count,
        &mut |bytes: ResolutionMemoryChangeV1| {
            bytes.apply_with(&mut outputs, |outputs| {
                retain_facts(count(facts_bytes.saturating_add(outputs))?)
            })
        },
    )?;
    for file in &mut facts {
        edges.extend(file.take_edges());
    }
    edges.rotate_left(cross_count);
    retain_facts(count(
        facts_bytes.saturating_add(outputs).saturating_add(
            edges
                .len()
                .div_ceil(2)
                .saturating_mul(std::mem::size_of::<CanonicalRelationEdgeV1>()),
        ),
    )?)?;
    edges.sort_by(edge_order);
    retained = facts
        .iter()
        .map(|file| file.retained_bytes())
        .sum::<usize>()
        .saturating_add(
            ordered
                .capacity()
                .saturating_mul(std::mem::size_of::<&PartitionedFileSegmentDescriptorV1>()),
        );
    let edge_bytes = super::resident_bytes::vec_bytes(
        &edges,
        edges.capacity(),
        super::resident_bytes::edge_heap_bytes,
    );
    let gap_bytes = super::resident_bytes::vec_bytes(
        &gaps,
        gaps.capacity(),
        super::resident_bytes::unresolved_heap_bytes,
    );
    let mut unresolved = super::resolution_outputs::unresolved_calls_for_edges_charged(
        &facts,
        &edges,
        &mut |bytes| {
            retain_facts(
                count(
                    retained
                        .saturating_add(edge_bytes)
                        .saturating_add(gap_bytes),
                )?
                .saturating_add(bytes),
            )
        },
    )?;
    let call_bytes = super::resident_bytes::vec_bytes(
        &unresolved,
        unresolved.capacity(),
        super::resident_bytes::unresolved_heap_bytes,
    );
    let mut live = retained
        .saturating_add(edge_bytes)
        .saturating_add(gap_bytes)
        .saturating_add(call_bytes);
    super::resolution_view::reserve_resolution_vec(
        &mut unresolved,
        gaps.len(),
        &mut |bytes: ResolutionMemoryChangeV1| {
            bytes.apply_with(&mut live, |live| retain_facts(count(live)?))
        },
    )?;
    unresolved.extend(gaps);
    retain_facts(count(
        live.saturating_add(
            unresolved
                .len()
                .div_ceil(2)
                .saturating_mul(std::mem::size_of::<
                    crate::chunks::CodeIndexUnresolvedReferenceV1,
                >()),
        ),
    )?)?;
    unresolved.sort();
    unresolved.dedup();
    let output_bytes = super::resident_bytes::vec_bytes(
        &unresolved,
        unresolved.capacity(),
        super::resident_bytes::unresolved_heap_bytes,
    )
    .saturating_add(edge_bytes);
    tracing::trace!(
        phase = "cross_resolution",
        retained_facts_bytes = retained,
        edge_count = edges.len(),
        unresolved_count = unresolved.len(),
        output_bytes,
        "cold resolution phase completed"
    );
    drop(cross_span);
    let evidence_span = tracing::trace_span!("code_index.cold.file_evidence").entered();
    let descriptor_bytes = ordered
        .len()
        .saturating_mul(
            std::mem::size_of::<PartitionedFileEvidenceDescriptorV1>() + "sha256:".len() + 64,
        )
        .saturating_add(
            ordered
                .iter()
                .map(|file| file.file_occurrence_id.as_str().len())
                .sum::<usize>(),
        );
    let phase_base = retained
        .saturating_add(output_bytes)
        .saturating_add(descriptor_bytes);
    retain_facts(count(phase_base)?)?;
    tracing::trace!(
        phase = "file_evidence",
        retained_base_bytes = phase_base,
        descriptor_bound_bytes = descriptor_bytes,
        "cold resolution phase started"
    );
    let mut file_evidence = Vec::with_capacity(ordered.len());
    super::file_evidence_rows::for_each_cold_file_evidence(
        &facts,
        &edges,
        &unresolved,
        &manifest.generation_id,
        &mut |workspace| retain_facts(count(phase_base)?.saturating_add(workspace)),
        &mut |index, evidence, workspace, retain| {
            super::lexical_page_source::checkpoint(control)?;
            if evidence.is_empty() {
                return Ok(());
            }
            let file = ordered[index];
            let (descriptor, bytes) =
                super::partitioned_codec::encode_file_evidence_segment_charged(
                    file.file_key,
                    &file.file_occurrence_id,
                    evidence,
                    &mut |encoding| retain(workspace.saturating_add(encoding)),
                )?;
            publish(SealedGenerationSegmentPublicationV1::FileEvidence {
                digest: &descriptor.segment_digest,
                bytes: &bytes,
            })?;
            file_evidence.push(descriptor);
            Ok(())
        },
    )?;
    file_evidence.sort_by_key(|descriptor| descriptor.file_key);
    tracing::trace!(
        phase = "file_evidence",
        descriptors = file_evidence.len(),
        retained_base_bytes = phase_base,
        "cold resolution phase completed"
    );
    drop(evidence_span);
    let index_span = tracing::trace_span!("code_index.cold.resolution_index").entered();
    tracing::trace!(
        phase = "resolution_index",
        retained_base_bytes = phase_base,
        "cold resolution phase started"
    );
    let resolution_index = super::resolution_index::seal_resolution_index_charged(
        &facts,
        &mut |workspace| retain_facts(count(phase_base)?.saturating_add(workspace)),
        &mut *publish,
    )?;
    tracing::trace!(
        phase = "resolution_index",
        segments = resolution_index.segments().count(),
        retained_base_bytes = phase_base,
        "cold resolution phase completed"
    );
    drop(index_span);
    // Graph pages decode their own bounded file window. Keep only occurrence
    // ownership here; symbol records and binding rows have no remaining reader.
    let roster_body = facts
        .len()
        .checked_mul(std::mem::size_of::<
            Vec<tracedecay_domain::SymbolOccurrenceId>,
        >())
        .ok_or_else(|| contract("graph owner roster bytes overflowed"))?;
    let roster_heaps = facts
        .iter()
        .map(|file| file.graph_owner_bytes())
        .sum::<usize>();
    retain_facts(count(phase_base.saturating_add(roster_body))?)?;
    let mut graph_owners = Vec::with_capacity(facts.len());
    for file in facts {
        graph_owners.push(file.into_graph_owners());
    }
    let phase_base = phase_base
        .checked_sub(retained)
        .ok_or_else(|| contract("graph base lacks resolution facts"))?
        .saturating_add(roster_body)
        .saturating_add(roster_heaps)
        .saturating_add(ordered_bytes);
    retain_facts(count(phase_base)?)?;
    let graph_span = tracing::trace_span!("code_index.cold.graph").entered();
    let phase_base = phase_base.saturating_add(
        resolution_index
            .segments()
            .map(|segment| {
                std::mem::size_of::<super::resolution_index::PartitionedIndexSegmentDescriptorV1>()
                    .saturating_add(segment.segment_digest.as_str().len())
            })
            .sum::<usize>(),
    );
    let owner_rows = graph_owners.iter().map(Vec::len).sum::<usize>();
    let owner_bytes = super::resident_bytes::btree_bytes(
        owner_rows,
        std::mem::size_of::<(&tracedecay_domain::SymbolOccurrenceId, &FileOccurrenceId)>(),
    );
    tracing::trace!(
        phase = "graph",
        graph_owner_count = owner_rows,
        owner_map_bytes = owner_bytes,
        retained_base_bytes = phase_base,
        "cold resolution phase started"
    );
    retain_facts(count(phase_base.saturating_add(owner_bytes))?)?;
    let mut owners = BTreeMap::new();
    for (file_owners, descriptor) in graph_owners.iter().zip(&ordered) {
        for occurrence in file_owners {
            match owners.insert(occurrence, &descriptor.file_occurrence_id) {
                Some(prior) if prior != &descriptor.file_occurrence_id => {
                    return Err(contract(
                        "one graph symbol occurrence belongs to multiple files",
                    ));
                }
                _ => {}
            }
        }
    }
    let graph_descriptors = snapshot
        .files
        .len()
        .saturating_mul(
            std::mem::size_of::<PartitionedCodeGraphPageDescriptorV1>() + "sha256:".len() + 64,
        )
        .saturating_add(
            snapshot
                .files
                .iter()
                .map(|file| {
                    file.file_occurrence_id
                        .as_str()
                        .len()
                        .saturating_add(file.logical_path.len())
                })
                .sum::<usize>(),
        );
    let key_bytes = super::resident_bytes::hash_table_bytes::<(&FileOccurrenceId, u32)>(
        super::helpers::resolution_hash_capacity(snapshot.files.len()),
    );
    let phase_base = phase_base
        .saturating_add(graph_descriptors)
        .saturating_add(key_bytes);
    retain_facts(count(phase_base.saturating_add(owner_bytes))?)?;
    let keys = snapshot
        .files
        .iter()
        .enumerate()
        .map(|(key, file)| {
            Ok((
                &file.file_occurrence_id,
                u32::try_from(key).map_err(|_| contract("code graph page file key exceeds u32"))?,
            ))
        })
        .collect::<Result<HashMap<_, _>, CodeIndexProductionErrorV1>>()?;
    let mut code_graph_pages = Vec::with_capacity(snapshot.files.len());
    super::graph_pages::for_each_code_graph_page(
        snapshot,
        &manifest.generation_id,
        &owners,
        &edges,
        &unresolved,
        &mut |workspace| {
            retain_facts(count(phase_base.saturating_add(owner_bytes))?.saturating_add(workspace))
        },
        &mut |file| {
            super::lexical_page_source::checkpoint(control)?;
            if file.disposition != SnapshotFileDispositionV1::Present {
                return Ok(None);
            }
            let key = keys
                .get(&file.file_occurrence_id)
                .ok_or_else(|| contract("code graph file is outside its snapshot"))?;
            source.decode_file(*key).map(Some)
        },
        &mut |page| {
            super::lexical_page_source::checkpoint(control)?;
            code_graph_pages.push(publish_code_graph_page(page, &BTreeSet::new(), publish)?);
            if code_graph_pages.len().is_power_of_two() {
                tracing::trace!(
                    phase = "graph",
                    completed_pages = code_graph_pages.len(),
                    total_files = snapshot.files.len(),
                    "cold graph publication progress"
                );
            }
            Ok(())
        },
    )?;
    tracing::trace!(
        phase = "graph",
        descriptors = code_graph_pages.len(),
        owner_map_bytes = owner_bytes,
        retained_base_bytes = phase_base,
        "cold resolution phase completed"
    );
    drop(graph_span);
    let statistics = super::CodeIndexGenerationStatisticsV1 {
        source_total_bytes,
        symbol_count,
        edge_count: count(edges.len())?,
        ambiguous_name_drops,
    };
    Ok(StagedResolutionPartsV1 {
        file_evidence,
        code_graph_pages,
        resolution_index,
        coverage,
        statistics,
    })
}
