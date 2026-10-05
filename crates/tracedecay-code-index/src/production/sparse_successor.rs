//! The successor's sealed parts, rebuilt only where an edit reaches.
//!
//! [`SealedSuccessorV1`] reads the successor's file set against its parent:
//! it re-decides the edited files' resolution over a view that decodes a
//! carried file only when a lookup lands in it, then seals each present
//! file's segment, evidence, and graph page, encoding anew what the edit or
//! its resolution touched and rekeying every other parent descriptor.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

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
use super::sealed_codec::FileScopeIdentityV1;
use super::sealed_parent::{
    DecodeFailureV1, SealedParentGenerationV1, SparseFileSourceV1, SparseFileV1,
};
use super::sparse_increment::{EditedFileV1, contract, count};
use super::sparse_resolution::{ResolvedFileV1, SparseSymbolsByNameV1, resolve_edit};
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
    failure: DecodeFailureV1,
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
            failure: DecodeFailureV1::default(),
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
        let stand_in = edited
            .first()
            .map(|file| &file.after)
            .ok_or_else(|| contract("a sparse resolution view needs an edited file"))?;
        self.present
            .iter()
            .map(|file| {
                let language = file
                    .language
                    .as_ref()
                    .map(|language| language.as_str())
                    .ok_or_else(|| contract("present snapshot file has no declared language"))?;
                Ok(match edited_by_path.get(file.logical_path.as_str()) {
                    Some(after) => {
                        SparseFileV1::decoded(&file.logical_path, language, Arc::clone(after))
                    }
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
                            stand_in,
                            failure: &self.failure,
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
    ) -> Result<SuccessorResolutionV1, CodeIndexProductionErrorV1> {
        if edited.is_empty() {
            return Ok(SuccessorResolutionV1::default());
        }
        let view = self.view(parent, edited, false)?;
        let before_view = self.view(parent, edited, true)?;
        let index = parent.resolution_index()?;
        let before_by_name = SparseSymbolsByNameV1::new(
            &index,
            &self.index_of_path,
            std::iter::empty(),
            &self.failure,
        );
        let by_name = SparseSymbolsByNameV1::new(
            &index,
            &self.index_of_path,
            edited.iter().map(|file| {
                (
                    self.edited_index[file.file.logical_path.as_str()],
                    file.file.logical_path.as_str(),
                    &file.after,
                )
            }),
            &self.failure,
        );
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
            &pairs,
            &self.index_of_path,
            &self.occurrence_of_path,
            &self.parent_key_of_path,
        );
        // A stand-in for an undecodable file can trip a contract inside the
        // resolution; the decode failure is the cause and wins.
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        let resolution = resolution?;
        let mut files = BTreeMap::new();
        for (file_index, resolved) in resolution.files {
            let file = self.present[file_index];
            files.insert(
                file.logical_path.clone(),
                (Arc::clone(view[file_index].artifacts()), resolved),
            );
        }
        Ok(SuccessorResolutionV1 {
            files,
            ambiguous_before: resolution.ambiguous_before,
            ambiguous_after: resolution.ambiguous_after,
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
            let resolved = resolution.files.get(&file.logical_path);
            let edited_file = edited_by_path.get(file.logical_path.as_str());
            let evidence = if resolved.is_some() || edited_file.is_some() {
                if let Some(parent_evidence) = parent.file_evidence(parent_key)? {
                    counts.before = counts
                        .before
                        .saturating_add(count(parent_evidence.cross_file_edge_count())?);
                }
                let artifacts = match (resolved, edited_file) {
                    (_, Some(edited)) => Arc::clone(&edited.after),
                    (Some((artifacts, _)), None) => Arc::clone(artifacts),
                    (None, None) => return Err(contract("a re-decided file has no artifacts")),
                };
                let edges = resolved
                    .map(|(_, resolved)| {
                        resolved
                            .edges
                            .iter()
                            .map(|(edge, path, identity)| (edge, path.as_str(), identity))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                counts.after = counts.after.saturating_add(count(edges.len())?);
                let calls = resolved
                    .map(|(_, resolved)| resolved.unresolved_calls.iter().collect::<Vec<_>>())
                    .unwrap_or_default();
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
            let resolved = resolution.files.get(&file.logical_path);
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
                (None, Some((artifacts, _))) => Some(Arc::clone(artifacts)),
                (None, None) if file.disposition == SnapshotFileDispositionV1::Present => {
                    return Err(contract(
                        "a present file's graph page changed without its artifacts",
                    ));
                }
                (None, None) => None,
            };
            let inputs = match artifacts.as_deref() {
                Some(artifacts) => {
                    let cross_file = resolved
                        .map(|(_, resolved)| resolved.edges.as_slice())
                        .unwrap_or_default();
                    self.page_inputs(
                        key,
                        file,
                        artifacts,
                        cross_file,
                        resolved
                            .map(|(_, resolved)| resolved.unresolved_calls.clone())
                            .unwrap_or_default(),
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

/// The files whose cross-file evidence the edit re-decided, by logical path,
/// with the artifacts resolution read them as.
#[derive(Default)]
pub(super) struct SuccessorResolutionV1 {
    pub(super) files: BTreeMap<String, (Arc<FileGenerationArtifactsV1>, ResolvedFileV1)>,
    pub(super) ambiguous_before: u64,
    pub(super) ambiguous_after: u64,
}
